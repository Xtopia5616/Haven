//! Aggregate application config (AppConfig / SettingsSnapshot) and the TOML
//! ConfigLoader. Both structs are generated from one shared field list by
//! the config_snapshot_pair! macro so the runtime and read snapshot cannot drift.

use super::*;

macro_rules! config_snapshot_pair {
    ($settings:ident; $(
        $(#[$field_doc:meta])*
        $field:ident: $ty:ty
    ),* $(,)?; config_only { $(
        $(#[$private_field_doc:meta])*
        $private_field:ident: $private_ty:ty
    ),* $(,)? } ; $($sanitize:tt)*) => {
        #[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
        #[serde(default, deny_unknown_fields)]
        pub struct AppConfig {
            $( $(#[$field_doc])* pub $field: $ty, )*
            $( $(#[$private_field_doc])* pub $private_field: $private_ty, )*
        }

        #[derive(Debug, Clone, Serialize, Deserialize, Default)]
        #[serde(default, deny_unknown_fields)]
        pub struct SettingsSnapshot {
            $( $(#[$field_doc])* pub $field: $ty, )*
        }

        impl From<&AppConfig> for SettingsSnapshot {
            fn from(c: &AppConfig) -> Self {
                let mut $settings = Self {
                    $( $field: c.$field.clone(), )*
                };
                $($sanitize)*
                $settings
            }
        }

        impl From<&SettingsSnapshot> for AppConfig {
            fn from(settings: &SettingsSnapshot) -> Self {
                Self {
                    $( $field: settings.$field.clone(), )*
                    $( $private_field: Default::default(), )*
                }
            }
        }

    };
}

config_snapshot_pair! {
    settings;
    /// Default shell for the agent's `shell` tool when the model omits the
    /// `shell` argument. One of `powershell` (built-in Windows PowerShell),
    /// `cmd`, or `pwsh` (PowerShell 7, requires a separate install).
    default_shell: ShellChoice,
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
    ; config_only {
        /// User-level lifecycle commands. This executable configuration is
        /// never exposed through Settings IPC.
        hooks: LifecycleHooksConfig,
        /// Execution workspace roots whose project `.haven/hooks.toml` files
        /// may contribute lifecycle commands. Trust is configured locally in
        /// config.toml and is never exposed through Settings IPC.
        trusted_hook_workspaces: Vec<PathBuf>,
        /// Local language servers available to code intelligence operations.
        /// This executable configuration is managed in config.toml and is never
        /// exposed through Settings IPC.
        lsp_servers: Vec<LspServerConfig>,
        /// Optional Everything CLI integration for filename search. The
        /// executable path is local-only and never exposed through Settings IPC.
        everything: EverythingConfig,
    } ;
    for p in settings.llm.providers.iter_mut() {
        p.api_key = String::new();
    }
    settings.media.ocr.api_key = String::new();
    settings.media.ocr.api_secret = String::new();
    for server in settings.mcp_servers.iter_mut() {
        server.env.clear();
    }
}

/// SettingsPatch write contract. Omitted sections remain untouched; nested `Option`
/// fields retain their explicit `null` meaning for clearing a configured value.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct SettingsPatch {
    pub default_shell: Option<ShellChoice>,
    pub llm: Option<LlmConfig>,
    pub hotkey: Option<HotkeyConfig>,
    pub session: Option<SessionConfig>,
    pub context_limits: Option<ContextLimitsConfig>,
    pub memory: Option<MemoryConfig>,
    pub security: Option<SecurityConfig>,
    pub media: Option<MediaConfig>,
    pub notification: Option<NotificationConfig>,
    pub log: Option<LogConfig>,
}

impl From<&SettingsSnapshot> for SettingsPatch {
    fn from(snapshot: &SettingsSnapshot) -> Self {
        Self {
            default_shell: Some(snapshot.default_shell),
            llm: Some(snapshot.llm.clone()),
            hotkey: Some(snapshot.hotkey.clone()),
            session: Some(snapshot.session.clone()),
            context_limits: Some(snapshot.context_limits.clone()),
            memory: Some(snapshot.memory.clone()),
            security: Some(snapshot.security.clone()),
            media: Some(snapshot.media.clone()),
            notification: Some(snapshot.notification.clone()),
            log: Some(snapshot.log.clone()),
        }
    }
}

// ---------------------------------------------------------------------------
// Config loader
// ---------------------------------------------------------------------------

/// TOML codec and file boundary held by [`ConfigService`](super::ConfigService).
/// Production configuration writes go through that owner so validation,
/// versioning, persistence, and change notifications stay together.
///
/// The mutable loader surface is intentionally unavailable to downstream
/// crates:
///
/// ```compile_fail
/// use haven_common::config::ConfigLoader;
/// let mut loader = ConfigLoader::load().unwrap();
/// loader.config_mut();
/// ```
///
/// ```compile_fail
/// use haven_common::config::ConfigLoader;
/// let mut loader = ConfigLoader::load().unwrap();
/// loader.persist().unwrap();
/// ```
///
/// ```compile_fail
/// use haven_common::config::{ConfigLoader, SettingsPatch};
/// let mut loader = ConfigLoader::load().unwrap();
/// loader.apply_settings(&SettingsPatch::default());
/// ```
#[derive(Debug, Clone)]
pub struct ConfigLoader {
    path: PathBuf,
    config: AppConfig,
    source_fingerprint: Option<u64>,
    recovery_required: bool,
}

/// Non-cryptographic freshness marker only; the file is still protected by
/// normal credential handling, while this prevents a stale process snapshot
/// from overwriting an out-of-band edit.
fn fingerprint_config_file(content: &[u8]) -> u64 {
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in content {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

/// Backup path with a timestamp so a failed or re-corrupted config never
/// overwrites the last usable recovery copy.
fn timestamped_backup_path(path: &Path) -> PathBuf {
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    static BACKUP_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let sequence = BACKUP_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let base = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("config");
    let pid = std::process::id();
    let mut backup = path.with_file_name(format!("{base}.toml.{ts}.{pid}.{sequence}.bak"));
    // The process id + sequence makes repeated parse failures in the same
    // second independent recovery points. Keep the existence check as a
    // final guard for unusual process-id reuse or a pre-existing file.
    while backup.exists() {
        let next = BACKUP_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        backup = path.with_file_name(format!("{base}.toml.{ts}.{pid}.{next}.bak"));
    }
    backup
}

const MAX_AUTOMATIC_CONFIG_BACKUPS: usize = 10;
/// Config input is intentionally small: it is a user-edited settings document,
/// not a bulk data format. Oversized files are rejected and left untouched.
const MAX_CONFIG_FILE_BYTES: usize = 4 * 1024 * 1024;

fn read_config_file(path: &Path) -> anyhow::Result<String> {
    use std::io::Read;

    let file = std::fs::File::open(path)?;
    if file.metadata()?.len() > MAX_CONFIG_FILE_BYTES as u64 {
        anyhow::bail!("config file exceeds the {MAX_CONFIG_FILE_BYTES}-byte limit");
    }

    // Read one byte past the limit so a file that grows after metadata is
    // checked is rejected without an unbounded allocation.
    let mut bytes = Vec::with_capacity(MAX_CONFIG_FILE_BYTES.min(64 * 1024));
    file.take(MAX_CONFIG_FILE_BYTES as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_CONFIG_FILE_BYTES {
        anyhow::bail!("config file exceeds the {MAX_CONFIG_FILE_BYTES}-byte limit");
    }
    String::from_utf8(bytes).map_err(Into::into)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct AutomaticBackupIdentity {
    timestamp_secs: u64,
    process_id: u32,
    sequence: u64,
}

struct TimestampedBackup {
    identity: AutomaticBackupIdentity,
    path: PathBuf,
}

fn parse_automatic_backup_identity(
    path: &Path,
    config_path: &Path,
) -> Option<AutomaticBackupIdentity> {
    let base = config_path.file_stem()?.to_str()?;
    let file_name = path.file_name()?.to_str()?;
    let middle = file_name
        .strip_prefix(&format!("{base}.toml."))?
        .strip_suffix(".bak")?;
    let parts: Vec<_> = middle.split('.').collect();
    // Current format: timestamp, process id, per-process sequence.
    let [timestamp, pid, sequence] = parts.as_slice() else {
        return None;
    };
    Some(AutomaticBackupIdentity {
        timestamp_secs: timestamp.parse().ok()?,
        process_id: pid.parse().ok()?,
        sequence: sequence.parse().ok()?,
    })
}

/// Keep only the newest automatic recovery copies for this config. Manual
/// backups and unrelated files are deliberately excluded by the strict name
/// parser above.
fn prune_timestamped_backups(config_path: &Path) {
    let Some(parent) = config_path.parent() else {
        return;
    };
    let Ok(entries) = std::fs::read_dir(parent) else {
        return;
    };
    let mut backups: Vec<_> = entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let path = entry.path();
            parse_automatic_backup_identity(&path, config_path)
                .map(|identity| TimestampedBackup { identity, path })
        })
        .collect();
    backups.sort_by_key(|backup| std::cmp::Reverse(backup.identity));
    for backup in backups.into_iter().skip(MAX_AUTOMATIC_CONFIG_BACKUPS) {
        if let Err(error) = std::fs::remove_file(&backup.path) {
            tracing::warn!(
                error = %crate::error::sanitize_error_text(&error.to_string()),
                "failed to prune old automatic config backup"
            );
        }
    }
}

fn backup_unparsable_config(path: &Path, content: &[u8], failure_kind: &'static str) {
    // Never silently start with defaults: the next persist() would overwrite the
    // user's config (MCP servers, API keys, skills) with defaults. Back up the
    // unparsable file so it can be recovered, then continue with defaults.
    let backup = timestamped_backup_path(path);
    // Back up exactly the already-bounded bytes that failed validation. This
    // avoids re-reading a concurrently replaced or grown config file.
    match std::fs::write(&backup, content) {
        Ok(_) => {
            prune_timestamped_backups(path);
            tracing::error!(
                failure_kind,
                "config parse failed; original config backup created"
            )
        }
        Err(backup_error) => tracing::error!(
            failure_kind,
            backup_error = %crate::error::sanitize_error_text(&backup_error.to_string()),
            "config parse failed and backup could not be created"
        ),
    }
}

/// Reject semantic config errors before accepting a parsed TOML value.
fn invalid_config_entry(config: &AppConfig) -> Option<&'static str> {
    if super::validate_app_config(config).is_err() {
        return Some("invalid application configuration contract");
    }
    if config
        .llm
        .providers
        .iter()
        .any(|provider| !provider.api_key.is_empty())
    {
        return Some("plaintext provider credential in config.toml");
    }
    if !config.media.ocr.api_key.is_empty() || !config.media.ocr.api_secret.is_empty() {
        return Some("plaintext OCR credential in config.toml");
    }
    if config
        .mcp_servers
        .iter()
        .any(|server| !server.env.is_empty())
    {
        return Some("plaintext MCP environment in config.toml");
    }
    None
}

/// Rewrite persisted capability and per-operation settings keys after builtin
/// operation names change. An explicit canonical setting wins a collision;
/// disabled state and deny decisions are merged conservatively.
fn migrate_builtin_tool_names(config: &mut AppConfig) -> bool {
    let mut changed = false;

    let mut permissions = Vec::<StoredPermission>::with_capacity(config.security.permissions.len());
    let mut permission_indexes = HashMap::<String, usize>::new();
    for mut permission in config.security.permissions.drain(..) {
        let canonical_key = crate::tools::canonical_builtin_tool_name(&permission.key);
        if canonical_key != permission.key {
            permission.key = canonical_key.to_owned();
            changed = true;
        }
        if let Some(index) = permission_indexes.get(&permission.key).copied() {
            // A denial always wins if a legacy key and its canonical key both
            // exist. This prevents a name migration from widening access.
            if permission.effect == crate::types::PermissionEffect::Deny {
                permissions[index].effect = crate::types::PermissionEffect::Deny;
            }
            changed = true;
        } else {
            permission_indexes.insert(permission.key.clone(), permissions.len());
            permissions.push(permission);
        }
    }
    config.security.permissions = permissions;

    let mut tool_settings = std::mem::take(&mut config.tool_settings)
        .into_iter()
        .collect::<Vec<_>>();
    // Keep an explicitly canonical entry as the source for non-boolean
    // settings when both spellings are present.
    tool_settings.sort_by_key(|(name, _)| crate::tools::canonical_builtin_tool_name(name) != name);
    let mut migrated_settings = HashMap::with_capacity(tool_settings.len());
    for (name, setting) in tool_settings {
        let canonical_name = crate::tools::canonical_builtin_tool_name(&name);
        if canonical_name != name {
            changed = true;
        }
        match migrated_settings.entry(canonical_name.to_owned()) {
            std::collections::hash_map::Entry::Vacant(entry) => {
                entry.insert(setting);
            }
            std::collections::hash_map::Entry::Occupied(mut entry) => {
                entry.get_mut().enabled &= setting.enabled;
                changed = true;
            }
        }
    }
    config.tool_settings = migrated_settings;

    changed
}

impl ConfigLoader {
    /// Returns the default config path under the application's data directory.
    pub fn default_path() -> PathBuf {
        Self::data_dir().join("config.toml")
    }

    /// Returns the Haven data directory. `HAVEN_DATA_DIR` can override the
    /// platform default (`%APPDATA%/haven` on Windows or
    /// `~/.local/share/haven` elsewhere) for isolated development or support
    /// runs. This remains the single source of truth for config, database,
    /// logs, media and skills.
    pub fn data_dir() -> PathBuf {
        data_dir_with_override(
            std::env::var_os("HAVEN_DATA_DIR")
                .filter(|path| !path.is_empty())
                .map(PathBuf::from),
            || {
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
            },
        )
    }

    /// Default skills directory: `<data_dir>/skills`.
    pub fn default_skills_dir() -> PathBuf {
        Self::data_dir().join("skills")
    }

    /// Load config from a specific path. Creates a default config file if missing.
    /// This path override is for tests and support tooling only; production
    /// loads from the canonical path and delegates writes to `ConfigService`.
    #[cfg(any(test, feature = "test-support"))]
    pub fn load_from(path: &Path) -> anyhow::Result<Self> {
        Self::load_from_path(path)
    }

    fn load_from_path(path: &Path) -> anyhow::Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        if !path.exists() {
            tracing::info!("config not found; creating default config");
            let default_cfg = AppConfig::default();
            let toml_str = toml::to_string_pretty(&default_cfg)?;
            let source_fingerprint = fingerprint_config_file(toml_str.as_bytes());
            std::fs::write(path, toml_str)?;
            return Ok(Self {
                path: path.to_path_buf(),
                config: default_cfg,
                source_fingerprint: Some(source_fingerprint),
                recovery_required: false,
            });
        }
        tracing::info!("loading config");
        let content = read_config_file(path)?;
        let source_fingerprint = Some(fingerprint_config_file(content.as_bytes()));
        let (config, recovery_required) = match toml::from_str::<AppConfig>(&content) {
            Ok(mut config) => {
                if config.context_limits.max_known_facts > MAX_FACTS_IN_EXTRACTION_PROMPT {
                    tracing::warn!(
                        configured = config.context_limits.max_known_facts,
                        capped = MAX_FACTS_IN_EXTRACTION_PROMPT,
                        "max_known_facts exceeds the extraction prompt limit; capping loaded value"
                    );
                    config.context_limits.max_known_facts = MAX_FACTS_IN_EXTRACTION_PROMPT;
                }
                if invalid_config_entry(&config).is_some() {
                    backup_unparsable_config(path, content.as_bytes(), "semantic_validation");
                    (AppConfig::default(), true)
                } else {
                    (config, false)
                }
            }
            Err(_) => {
                backup_unparsable_config(path, content.as_bytes(), "toml_parse");
                (AppConfig::default(), true)
            }
        };
        let mut loader = Self {
            path: path.to_path_buf(),
            config,
            source_fingerprint,
            recovery_required,
        };
        if !loader.recovery_required && migrate_builtin_tool_names(&mut loader.config) {
            tracing::info!("migrating stored builtin tool names in config.toml");
            if let Err(error) = loader.persist() {
                tracing::warn!(
                    error = %crate::error::sanitize_error_text(&error.to_string()),
                    "failed to persist builtin tool name migration; canonical names remain active in memory"
                );
            }
        }
        Ok(loader)
    }

    /// Load from the default path.
    pub fn load() -> anyhow::Result<Self> {
        Self::load_from_path(&Self::default_path())
    }

    /// Write the current config through a same-directory temporary file and
    /// replace the configured path only after the complete TOML write succeeds.
    /// A successful return confirms the replacement call succeeded; it does
    /// not guarantee survival across an OS crash or sudden power loss.
    pub(crate) fn persist(&mut self) -> anyhow::Result<()> {
        self.ensure_writable()?;
        super::validate_app_config(&self.config)?;
        super::credentials::validate_config_references(&self.config)?;
        let toml_str = toml::to_string_pretty(&self.config)?;
        anyhow::ensure!(
            toml_str.len() <= MAX_CONFIG_FILE_BYTES,
            "serialized config exceeds the {MAX_CONFIG_FILE_BYTES}-byte limit"
        );
        static PERSIST_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let file_name = self
            .path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("config.toml");
        let serial = PERSIST_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let tmp_path = self.path.with_file_name(format!(
            ".{file_name}.tmp.{}.{}",
            std::process::id(),
            serial
        ));
        if let Err(error) = std::fs::write(&tmp_path, &toml_str) {
            let _ = std::fs::remove_file(&tmp_path);
            return Err(error.into());
        }
        if let Some(expected) = self.source_fingerprint {
            let current = match read_config_file(&self.path) {
                Ok(content) => fingerprint_config_file(content.as_bytes()),
                Err(error) => {
                    let _ = std::fs::remove_file(&tmp_path);
                    return Err(error);
                }
            };
            if current != expected {
                let _ = std::fs::remove_file(&tmp_path);
                anyhow::bail!(
                    "config file changed outside Haven; restart Haven to load the latest settings"
                );
            }
        }
        if let Err(error) = std::fs::rename(&tmp_path, &self.path) {
            let _ = std::fs::remove_file(&tmp_path);
            return Err(error.into());
        }
        self.source_fingerprint = Some(fingerprint_config_file(toml_str.as_bytes()));
        Ok(())
    }

    pub fn config(&self) -> &AppConfig {
        &self.config
    }

    pub(crate) fn config_mut(&mut self) -> &mut AppConfig {
        &mut self.config
    }

    /// Apply a frontend `SettingsPatch` update in loader unit tests.
    #[cfg(test)]
    fn apply_settings(&mut self, settings: &SettingsPatch) {
        self.config.apply_settings_patch(settings);
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn ensure_writable(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            !self.recovery_required,
            "配置文件解析或校验失败，当前以默认值只读恢复；请修复或恢复 config.toml 备份，或删除 config.toml 后重启再编辑"
        );
        Ok(())
    }
}

impl AppConfig {
    /// Apply only submitted SettingsPatch sections, preserving stored api keys when
    /// the incoming value is empty.
    pub fn apply_settings_patch(&mut self, settings: &SettingsPatch) {
        // Preserve existing keys if the frontend sends empty strings (masked).
        // `SettingsSnapshot::from` blanks every api key, so the incoming `llm` carries
        // no secrets; the previous live config is the only key source.
        let prev_llm = self.llm.clone();

        // Replace the whole `llm` section wholesale instead of copying fields
        // by hand: a new field added to `LlmConfig` is then applied here
        // automatically and cannot be forgotten (the two structs stay in sync
        // via `From<&AppConfig> for SettingsSnapshot`, which masks keys only).
        if let Some(settings_llm) = &settings.llm {
            let mut llm = settings_llm.clone();
            // Preserve masked provider keys, matched by provider name. Roles never
            // carry keys (the api_key lives on the referenced provider), so no
            // model-level key preservation is needed.
            for prov in llm.providers.iter_mut() {
                if prov.api_key.is_empty()
                    && let Some(prev) = prev_llm.providers.iter().find(|p| p.name == prov.name)
                {
                    prov.api_key = prev.api_key.clone();
                }
            }
            // Drop incomplete named models so request policies cannot select a
            // half-written provider/model assignment.
            llm.models.retain(|model_config| {
                model_config.is_assigned() && !model_config.id.trim().is_empty()
            });
            self.llm = llm;
        }

        if let Some(default_shell) = settings.default_shell {
            self.default_shell = default_shell;
        }
        if let Some(hotkey) = &settings.hotkey {
            self.hotkey = hotkey.clone();
        }
        if let Some(session) = &settings.session {
            self.session = session.clone();
        }
        // The settings form sends the full `context_limits` object (the
        // frontend keeps the loaded copy intact and only edits exposed
        // fields), so applying it here cannot wipe fields the UI does not
        // render. `#[serde(default)]` fills any genuinely missing field with
        // its default, which is the current schema's partial-settings contract.
        if let Some(context_limits) = &settings.context_limits {
            self.context_limits = context_limits.clone();
        }
        if let Some(memory) = &settings.memory {
            self.memory = memory.clone();
        }
        // Permanent permissions are mutated by resolve_confirmation /
        // revoke_permission, not the settings form. The form may hold a stale
        // snapshot (Always grant while SettingsSnapshot was open) — overwriting would
        // wipe live grants. Keep on-disk permissions; apply the selected
        // security boundaries and confirmation mode.
        if let Some(security) = &settings.security {
            let prev_permissions = self.security.permissions.clone();
            let prev_encrypt = self.security.encrypt_sensitive;
            self.security = security.clone();
            self.security.permissions = prev_permissions;
            // encrypt_sensitive has no UI yet; don't let the form force `true`.
            self.security.encrypt_sensitive = prev_encrypt;
        }
        if let Some(settings_media) = &settings.media {
            self.media = {
                let mut media = settings_media.clone();
                let prev = &self.media;
                // OCR remains a dedicated provider with its own credentials; keep
                // masked values when the settings form sends empty strings.
                if media.ocr.api_key.is_empty() {
                    media.ocr.api_key = prev.ocr.api_key.clone();
                }
                if media.ocr.api_secret.is_empty() {
                    media.ocr.api_secret = prev.ocr.api_secret.clone();
                }
                media
            };
        }
        // The settings form does not manage these sections — MCP servers are
        // mutated by their own bridge commands and the `self` tool, skills and
        // tool settings likewise. The frontend payload omits them, so with
        // `#[serde(default)]` they deserialize to empty/default values here;
        // overwriting the live config with them would wipe every configured
        // MCP server / skill / tool setting on each settings save. Keep the
        // current values; specialized typed patches can update these sections
        // through `ConfigService` without allowing the settings form to wipe
        // them.
        if let Some(log) = &settings.log {
            self.log = log.clone();
        }
        if let Some(notification) = &settings.notification {
            self.notification = notification.clone();
        }
    }
}

fn data_dir_with_override(
    override_path: Option<PathBuf>,
    default_path: impl FnOnce() -> PathBuf,
) -> PathBuf {
    override_path.unwrap_or_else(default_path)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn data_directory_override_skips_the_platform_default() {
        let override_path = PathBuf::from("isolated-data-root");
        let default_was_called = std::cell::Cell::new(false);

        let selected = data_dir_with_override(Some(override_path.clone()), || {
            default_was_called.set(true);
            PathBuf::from("shared-data-root")
        });

        assert_eq!(selected, override_path);
        assert!(!default_was_called.get());

        let selected = data_dir_with_override(None, || PathBuf::from("platform-default"));
        assert_eq!(selected, PathBuf::from("platform-default"));
    }

    #[test]
    fn default_config_has_expected_values() {
        let cfg = AppConfig::default();
        assert_eq!(
            cfg.media.input_strategy,
            crate::media::MediaInputStrategy::Auto
        );
        assert_eq!(cfg.media.audio.max_duration_secs, 60);
        assert_eq!(cfg.hotkey.key_binding, "Ctrl+Shift+Space");
        assert!(cfg.notification.tool_run_completed.in_app);
        assert!(cfg.notification.tool_run_completed.windows);
        assert_eq!(cfg.session.max_concurrent, 3);
        assert_eq!(cfg.session.prompt_history_limit, 50);
        assert_eq!(cfg.session.max_steps_per_run, 500);
        assert_eq!(cfg.context_limits.compaction_ratio, 0.65);
        assert_eq!(cfg.context_limits.compaction_reserve_tokens, 8192);
        assert_eq!(cfg.context_limits.default_context_window, 64_000);
        assert_eq!(cfg.context_limits.response_token_floor, 32_000);
        assert_eq!(cfg.context_limits.incomplete_tool_args_retries, 2);
        assert_eq!(cfg.context_limits.stream_stall_warn_delay_ms, 10_000);
        assert_eq!(cfg.context_limits.reasoning_echo_max_chars, 1200);
        assert_eq!(cfg.context_limits.max_observation_chars, 16_000);
        assert_eq!(cfg.context_limits.max_transcript_chars, 4_000);
        assert_eq!(cfg.context_limits.max_attachment_images, 4);
        assert_eq!(cfg.context_limits.max_attachment_files, 5);
        assert_eq!(cfg.context_limits.max_upload_total_bytes, 512 * 1024 * 1024);
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
        assert_eq!(cfg.context_limits.file_max_full_file_bytes, 128_000);
        assert_eq!(cfg.context_limits.file_max_patch_bytes, 8 * 1024 * 1024);
        assert_eq!(cfg.context_limits.file_line_max_bytes, 128_000);
        assert_eq!(
            cfg.context_limits.file_byte_read_max_bytes,
            16 * 1024 * 1024
        );
        assert_eq!(cfg.context_limits.file_write_max_bytes, 16 * 1024 * 1024);
        assert_eq!(
            cfg.context_limits.mcp_max_binary_base64_bytes,
            2 * 1024 * 1024
        );
        assert_eq!(cfg.context_limits.mcp_max_sse_buffer_bytes, 2 * 1024 * 1024);
        assert_eq!(cfg.context_limits.skills_max_md_bytes, 256 * 1024);
        assert_eq!(cfg.context_limits.skills_max_parse_lines, 5_000);
        assert_eq!(cfg.context_limits.skills_max_line_bytes, 4_096);
        assert_eq!(cfg.context_limits.self_tool_max_script_bytes, 512 * 1024);
        assert_eq!(cfg.context_limits.network_max_retries, 2);
        assert_eq!(cfg.context_limits.network_max_body_bytes, 1024 * 1024);
        assert_eq!(cfg.context_limits.clipboard_history_max_entries, 100);
        assert_eq!(cfg.context_limits.max_running_background_tool_runs, 64);
        assert_eq!(cfg.context_limits.event_chunk_batch_max_bytes, 8 * 1024);
        assert_eq!(cfg.context_limits.input_ring_buffer_secs, 20);
        assert_eq!(cfg.context_limits.embedding_chunk_size, 10);
        assert_eq!(cfg.context_limits.max_tools_per_request, 256);
        assert_eq!(cfg.context_limits.partial_checkpoint_interval_secs, 2);
        assert_eq!(cfg.context_limits.fact_infer_interval_steps, 25);
        assert_eq!(cfg.session.history_retention_days, 90);
        assert!(cfg.security.encrypt_sensitive);
        assert!(cfg.mcp_servers.is_empty());
        assert_eq!(cfg.media.stt.provider_name, "llm");
        assert_eq!(cfg.media.stt.timeout_secs, 30);
        assert!(cfg.media.stt.mcp_server.is_none());
        assert!(cfg.media.stt.model.is_empty());
        assert_eq!(cfg.media.stt.min_confidence, 0.7);
        assert_eq!(cfg.media.ocr.provider, "llm");
        assert!(cfg.media.ocr.api_key.is_empty());
        assert!(cfg.media.ocr.api_secret.is_empty());
        assert_eq!(cfg.media.ocr.timeout_secs, 20);
        assert_eq!(cfg.media.ocr.min_confidence, 0.7);
        assert_eq!(cfg.media.tts.provider_name, "none");
        assert!(cfg.media.tts.voice.is_empty());
        assert_eq!(cfg.media.tts.timeout_secs, 60);
        assert_eq!(cfg.media.image_gen.provider_name, "none");
        assert_eq!(cfg.media.image_gen.timeout_secs, 120);
        assert_eq!(cfg.llm.max_concurrent_requests, 2);
        assert!(cfg.llm.providers.is_empty());
        assert!(cfg.llm.models.is_empty());
        assert!(cfg.llm.request_policies.is_empty());
        assert_eq!(cfg.llm.materialize(None, None), RouterConfig::default());
    }

    #[test]
    fn removed_response_retry_settings_are_rejected() {
        for field in [
            "cut_off_retries = 2",
            "empty_response_max_retries = 3",
            "empty_response_retry_delay_ms = 1500",
        ] {
            let toml = format!("[context_limits]\n{field}\n");
            assert!(
                toml::from_str::<AppConfig>(&toml).is_err(),
                "removed setting must not be accepted: {field}"
            );
        }
    }

    #[test]
    fn file_limits_use_byte_units_and_separate_read_and_write_caps() {
        for field in [
            "file_read_max_chars = 128000",
            "file_max_line_chars = 128000",
            "file_max_byte_read = 16777216",
        ] {
            let toml = format!("[context_limits]\n{field}\n");
            assert!(
                toml::from_str::<AppConfig>(&toml).is_err(),
                "retired file limit key must not be accepted: {field}"
            );
        }

        let parsed: AppConfig = toml::from_str(
            "[context_limits]\nfile_max_full_file_bytes = 65536\nfile_max_patch_bytes = 4194304\nfile_line_max_bytes = 8192\nfile_byte_read_max_bytes = 2097152\nfile_write_max_bytes = 4194304\n",
        )
        .unwrap();
        assert_eq!(parsed.context_limits.file_max_full_file_bytes, 65_536);
        assert_eq!(parsed.context_limits.file_max_patch_bytes, 4 * 1024 * 1024);
        assert_eq!(parsed.context_limits.file_line_max_bytes, 8_192);
        assert_eq!(
            parsed.context_limits.file_byte_read_max_bytes,
            2 * 1024 * 1024
        );
        assert_eq!(parsed.context_limits.file_write_max_bytes, 4 * 1024 * 1024);
    }

    #[test]
    fn mcp_binary_cap_names_the_base64_representation_it_measures() {
        let old = toml::from_str::<AppConfig>(
            "[context_limits]\nmcp_max_binary_payload_bytes = 2097152\n",
        );
        assert!(
            old.is_err(),
            "the ambiguous payload key must not be accepted"
        );

        let parsed: AppConfig =
            toml::from_str("[context_limits]\nmcp_max_binary_base64_bytes = 1048576\n").unwrap();
        assert_eq!(
            parsed.context_limits.mcp_max_binary_base64_bytes,
            1024 * 1024
        );
    }

    #[test]
    fn session_prompt_history_limit_uses_the_session_config_key() {
        let toml = "[session]\nprompt_history_limit = 73\n";
        let parsed: AppConfig = toml::from_str(toml).unwrap();
        assert_eq!(parsed.session.prompt_history_limit, 73);

        let serialized = toml::to_string_pretty(&parsed).unwrap();
        assert!(serialized.contains("prompt_history_limit = 73"));
        assert!(!serialized.contains("session_window_size"));
    }

    #[test]
    fn removed_memory_session_window_key_is_rejected() {
        let parsed = toml::from_str::<AppConfig>("[memory]\nsession_window_size = 73\n");
        assert!(
            parsed.is_err(),
            "the retired memory key must not be accepted as an alias"
        );
    }

    #[test]
    fn skill_line_byte_limit_uses_current_key_and_rejects_old_key() {
        let parsed =
            toml::from_str::<AppConfig>("[context_limits]\nskills_max_line_bytes = 2048\n")
                .unwrap();
        assert_eq!(parsed.context_limits.skills_max_line_bytes, 2048);

        let old_key = toml::from_str::<AppConfig>("[context_limits]\nskills_max_line_len = 2048\n");
        assert!(
            old_key.is_err(),
            "the retired character-length key must not be accepted as an alias"
        );
    }

    #[test]
    fn background_tool_run_limit_is_configurable_and_retired_schedule_limits_are_rejected() {
        let parsed = toml::from_str::<AppConfig>(
            "[context_limits]\nmax_running_background_tool_runs = 29\n",
        )
        .unwrap();
        assert_eq!(parsed.context_limits.max_running_background_tool_runs, 29);

        for old_key in [
            "scheduled_tool_runs_max",
            "background_max_tool_runs",
            "max_pending_scheduled_tool_runs",
            "scheduled_tool_runs_due_horizon_secs",
        ] {
            let old_config = format!("[context_limits]\n{old_key} = 12\n");
            assert!(
                toml::from_str::<AppConfig>(&old_config).is_err(),
                "retired ToolRun limit key {old_key} must not be accepted as an alias"
            );
        }
    }

    #[test]
    fn config_roundtrip_through_toml() {
        let mut cfg = AppConfig::default();
        cfg.notification.tool_run_completed = NotifyChannels {
            in_app: false,
            windows: true,
        };
        let s = toml::to_string_pretty(&cfg).unwrap();
        let parsed: AppConfig = toml::from_str(&s).unwrap();
        assert_eq!(cfg, parsed);
    }

    #[test]
    fn notification_config_rejects_removed_fields() {
        let parsed = toml::from_str::<NotificationConfig>(
            r#"
            [removed_channel]
            in_app = true
            windows = true
            "#,
        );

        assert!(
            parsed.is_err(),
            "removed notification fields must be rejected"
        );
    }

    #[test]
    fn security_explicit_permission_mode_uses_named_profile() {
        let parsed: SecurityConfig = toml::from_str(
            r#"
                permission_mode = "default"
                encrypt_sensitive = true
            "#,
        )
        .unwrap();
        assert_eq!(parsed.permission_mode, PermissionMode::Default);
    }

    #[test]
    fn security_explicit_auto_edit_mode_wins() {
        let parsed: SecurityConfig = toml::from_str(r#"permission_mode = "auto_edit""#).unwrap();
        assert_eq!(parsed.permission_mode, PermissionMode::AutoEdit);
    }

    #[test]
    fn security_missing_table_uses_default_mode() {
        assert_eq!(
            SecurityConfig::default().permission_mode,
            PermissionMode::Default
        );
        assert_eq!(SecurityConfig::default().network_policy, NetworkPolicy::Ask);
        assert_eq!(SecurityConfig::default().execution_root, None);
    }

    #[test]
    fn security_execution_root_round_trips_as_an_optional_path() {
        let directory = tempfile::tempdir().unwrap();
        let mut security = SecurityConfig::default();
        security.execution_root = Some(directory.path().to_path_buf());

        let encoded = toml::to_string(&security).unwrap();
        let decoded: SecurityConfig = toml::from_str(&encoded).unwrap();
        assert_eq!(decoded.execution_root, security.execution_root);
    }

    #[test]
    fn tool_config_missing_enabled_defaults_to_true() {
        // Existing config.toml files written before the `enabled` field
        // existed have `[tool_settings.*]` sections without `enabled`. It
        // must deserialize to `true` (tool stays enabled), never to the
        // `bool::default()` of false.
        let toml_str = r#"
            [tool_settings.files]
            timeout_secs = 60
        "#;
        let cfg: AppConfig = toml::from_str(toml_str).unwrap();
        let files = cfg.tool_settings.get("files").unwrap();
        assert!(files.enabled);
        assert_eq!(files.timeout_secs, Some(60));
        assert_eq!(files.max_retries, None);
        assert_eq!(files.retry_backoff_secs, None);
        // Per-tool output cap defaults to None → inherits the global
        // `context_limits.max_observation_chars`.
        assert_eq!(files.max_output_chars, None);
    }

    #[test]
    fn tool_config_without_timeout_preserves_intrinsic_default() {
        let cfg: AppConfig = toml::from_str(
            r#"
            [tool_settings.http]
            max_retries = 3
            "#,
        )
        .unwrap();
        let http = cfg.tool_settings.get("http").unwrap();
        assert_eq!(http.timeout_secs, None);
        assert_eq!(http.max_retries, Some(3));
    }

    #[test]
    fn settings_hide_provider_api_keys() {
        let mut cfg = AppConfig::default();
        cfg.llm.providers.push(ProviderConfig {
            name: "openai".into(),
            api_key: "super-secret".to_string(),
            ..Default::default()
        });
        let settings = SettingsSnapshot::from(&cfg);
        assert!(settings.llm.providers[0].api_key.is_empty());
        // Models never carry keys (they live on the provider).
        assert_eq!(settings.llm.models.len(), 0);
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
        llm.set_model_config(
            "small_model",
            ModelConfig {
                provider_name: "openai".into(),
                model: "gpt-4o-mini".into(),
                max_tokens: Some(4096),
                ..Default::default()
            },
        );
        llm.set_model_config(
            "default_model",
            ModelConfig {
                provider_name: "openai".into(),
                model: "gpt-4o".into(),
                max_tokens: Some(20_000),
                ..Default::default()
            },
        );
        let lifted = llm.materialize(Some(10_000), None);
        // The fast-chat model cap is raised to the floor.
        assert_eq!(
            lifted
                .model_config("small_model")
                .unwrap()
                .endpoint
                .max_tokens,
            10_000
        );
        // A model already above the floor keeps its own value.
        assert_eq!(
            lifted
                .model_config("default_model")
                .unwrap()
                .endpoint
                .max_tokens,
            20_000
        );
        // Materialized endpoints carry provider key + model id.
        assert_eq!(
            lifted
                .model_config("default_model")
                .unwrap()
                .endpoint
                .api_key,
            "key"
        );
        assert_eq!(
            lifted
                .model_config("default_model")
                .unwrap()
                .endpoint
                .model_name,
            "gpt-4o"
        );
    }

    #[test]
    fn materialize_fills_reasoning_echo_and_preserves_overrides() {
        let mut llm = LlmConfig::default();
        llm.providers.push(ProviderConfig {
            name: "openai".into(),
            api_key: "key".into(),
            ..Default::default()
        });
        llm.set_model_config(
            "default_model",
            ModelConfig {
                provider_name: "openai".into(),
                model: "gpt-4o".into(),
                reasoning_echo_max_chars: Some(1234),
                ..Default::default()
            },
        );
        llm.set_model_config(
            "small_model",
            ModelConfig {
                provider_name: "openai".into(),
                model: "gpt-4o-mini".into(),
                ..Default::default()
            },
        );
        let filled = llm.materialize(None, Some(5000));
        // An endpoint without an override inherits the global cap.
        assert_eq!(
            filled
                .model_config("small_model")
                .unwrap()
                .endpoint
                .reasoning_echo_max_chars,
            Some(5000)
        );
        // A per-model override is preserved.
        assert_eq!(
            filled
                .model_config("default_model")
                .unwrap()
                .endpoint
                .reasoning_echo_max_chars,
            Some(1234)
        );
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
        let mut settings = SettingsSnapshot::from(&cfg);
        // Frontend sends empty api keys (masked) but a new base URL / model.
        settings.llm.providers[0].base_url = "https://gateway.example/v1".to_string();
        settings.llm.providers[1].name = "renamed".to_string();
        settings.llm.models.push(ModelConfig {
            id: "default_model".into(),
            provider_name: "openai".into(),
            model: "new-model".into(),
            ..Default::default()
        });
        settings.llm.models.push(ModelConfig {
            id: "retired_model".into(),
            provider_name: "openai".into(),
            model: "old-model".into(),
            ..Default::default()
        });
        let mut loader = ConfigLoader {
            path: PathBuf::from("unused"),
            config: cfg,
            source_fingerprint: None,
            recovery_required: false,
        };
        loader.apply_settings(&SettingsPatch::from(&settings));
        // Keys preserved by provider name.
        assert_eq!(loader.config().llm.providers[0].api_key, "keep-me");
        assert_eq!(
            loader.config().llm.providers[0].base_url,
            "https://gateway.example/v1"
        );
        // A renamed provider loses the key (no stable name match), as expected.
        assert_eq!(loader.config().llm.providers[1].name, "renamed");
        assert!(loader.config().llm.providers[1].api_key.is_empty());
        // The named model assignment was applied.
        assert_eq!(
            loader
                .config()
                .llm
                .model_config("default_model")
                .unwrap()
                .model,
            "new-model"
        );
        // Named model assignments are independent of request policies; a
        // complete assignment remains available even when it is not currently
        // selected by a policy.
        assert!(
            loader
                .config()
                .llm
                .models
                .iter()
                .any(|model| model.id == "retired_model")
        );
    }

    #[test]
    fn apply_settings_preserves_ocr_credentials_when_empty() {
        let mut cfg = AppConfig::default();
        cfg.media.ocr.api_key = "keep-ocr-key".to_string();
        cfg.media.ocr.api_secret = "keep-ocr-secret".to_string();
        let mut settings = SettingsSnapshot::from(&cfg);
        // Frontend sends masked OCR credentials but new media models/voices.
        settings.media.stt.model = "whisper-1".to_string();
        settings.media.ocr.provider = "baidu".to_string();
        settings.media.tts.voice = "alloy".to_string();
        settings.media.image_gen.model = "gpt-image-1".to_string();
        let mut loader = ConfigLoader {
            path: PathBuf::from("unused"),
            config: cfg,
            source_fingerprint: None,
            recovery_required: false,
        };
        loader.apply_settings(&SettingsPatch::from(&settings));
        let media = &loader.config().media;
        assert_eq!(media.stt.model, "whisper-1");
        assert_eq!(media.ocr.api_key, "keep-ocr-key");
        assert_eq!(media.ocr.api_secret, "keep-ocr-secret");
        assert_eq!(media.ocr.provider, "baidu");
        assert_eq!(media.tts.voice, "alloy");
        assert_eq!(media.image_gen.model, "gpt-image-1");
    }

    #[test]
    fn settings_hide_media_api_keys() {
        let mut cfg = AppConfig::default();
        cfg.media.ocr.api_key = "ocr-secret".to_string();
        cfg.media.ocr.api_secret = "ocr-secret-2".to_string();
        let settings = SettingsSnapshot::from(&cfg);
        assert!(settings.media.ocr.api_key.is_empty());
        assert!(settings.media.ocr.api_secret.is_empty());
    }

    #[test]
    fn lsp_servers_round_trip_in_config_without_entering_settings_ipc() {
        let config: AppConfig = toml::from_str(
            r#"
                [[lsp_servers]]
                name = "rust-analyzer"
                command = "rust-analyzer"
                args = ["--log-file", "server.log"]
                extensions = [".rs"]
                language_id = "rust"
                enabled = true
            "#,
        )
        .unwrap();

        let round_trip: AppConfig = toml::from_str(&toml::to_string(&config).unwrap()).unwrap();
        assert_eq!(round_trip.lsp_servers, config.lsp_servers);

        let settings = SettingsSnapshot::from(&config);
        let settings_json = serde_json::to_value(&settings).unwrap();
        assert!(settings_json.get("lsp_servers").is_none());

        // Settings writes use a sparse patch and therefore cannot erase the
        // executable configuration owned by config.toml.
        let mut loader = ConfigLoader {
            path: PathBuf::from("unused"),
            config,
            source_fingerprint: None,
            recovery_required: false,
        };
        loader.apply_settings(&SettingsPatch::from(&settings));
        assert_eq!(loader.config().lsp_servers.len(), 1);
        assert_eq!(loader.config().lsp_servers[0].command, "rust-analyzer");
    }

    #[test]
    fn everything_cli_config_is_local_only_and_survives_settings_writes() {
        let config: AppConfig = toml::from_str(
            r#"
                [everything]
                enabled = true
                command = "C:\\Tools\\es.exe"
            "#,
        )
        .unwrap();
        let settings = SettingsSnapshot::from(&config);
        assert!(
            serde_json::to_value(settings)
                .unwrap()
                .get("everything")
                .is_none()
        );
        let mut loader = ConfigLoader {
            path: PathBuf::from("unused"),
            config,
            source_fingerprint: None,
            recovery_required: false,
        };
        loader.apply_settings(&SettingsPatch::default());
        assert!(loader.config().everything.enabled);
        assert_eq!(
            loader.config().everything.command.as_deref(),
            Some("C:\\Tools\\es.exe")
        );
    }

    #[test]
    fn lifecycle_hooks_and_project_trust_round_trip_outside_settings_ipc() {
        let directory = tempfile::tempdir().unwrap();
        let mut config = AppConfig::default();
        config.hooks.pre_tool_use.push(HookCommand {
            command: "pwsh.exe".into(),
            args: vec!["-NoProfile".into(), "-File".into(), "hook.ps1".into()],
            matcher: Some("shell".into()),
        });
        config
            .trusted_hook_workspaces
            .push(directory.path().to_path_buf());

        let encoded = toml::to_string(&config).unwrap();
        let round_trip: AppConfig = toml::from_str(&encoded).unwrap();
        assert_eq!(round_trip.hooks, config.hooks);
        assert_eq!(
            round_trip.trusted_hook_workspaces,
            config.trusted_hook_workspaces
        );

        let settings_json = serde_json::to_value(SettingsSnapshot::from(&config)).unwrap();
        assert!(settings_json.get("hooks").is_none());
        assert!(settings_json.get("trusted_hook_workspaces").is_none());

        let mut loader = ConfigLoader {
            path: PathBuf::from("unused"),
            config,
            source_fingerprint: None,
            recovery_required: false,
        };
        let settings = SettingsSnapshot::from(loader.config());
        loader.apply_settings(&SettingsPatch::from(&settings));
        assert_eq!(loader.config().hooks.pre_tool_use.len(), 1);
        assert_eq!(loader.config().trusted_hook_workspaces.len(), 1);
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
            "files".into(),
            ToolConfig {
                timeout_secs: Some(60),
                ..Default::default()
            },
        );
        cfg.mcp_discovery.health_interval_secs = 99;
        cfg.context_limits.max_transcript_chars = 9999;

        let mut settings = SettingsSnapshot::from(&cfg);
        // The settings form payload omits these sections → they deserialize
        // to empty/defaults (SettingsSnapshot derives #[serde(default)]).
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
            source_fingerprint: None,
            recovery_required: false,
        };
        loader.apply_settings(&SettingsPatch::from(&settings));

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
        loader.apply_settings(&SettingsPatch::from(&settings));
        assert_eq!(loader.config().context_limits.max_transcript_chars, 8888);
    }

    #[test]
    fn load_creates_default_file() {
        let dir = std::env::temp_dir().join(format!("haven_test_{}", uuid::Uuid::new_v4()));
        let path = dir.join("config.toml");
        let loader = ConfigLoader::load_from(&path).unwrap();
        assert!(path.exists());
        assert!(!loader.recovery_required);
        assert_eq!(loader.config().media.audio.max_duration_secs, 60);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn settings_patch_keeps_omitted_sections_and_clears_explicit_nullable_values() {
        let mut config = AppConfig::default();
        config.session.max_concurrent = 7;
        config.security.execution_root = Some("C:\\Haven\\Workspace".into());
        config.log.file_path = Some("C:\\Haven\\logs\\haven.log".into());

        let patch: SettingsPatch = serde_json::from_value(serde_json::json!({
            "security": { "execution_root": null }
        }))
        .unwrap();
        config.apply_settings_patch(&patch);

        assert_eq!(config.session.max_concurrent, 7);
        assert_eq!(config.security.execution_root, None);
        assert_eq!(
            config.log.file_path,
            Some(PathBuf::from("C:\\Haven\\logs\\haven.log"))
        );
    }

    #[test]
    fn compute_cost_usd_zero_rates_return_none() {
        let ep = ModelEndpoint::default();
        assert_eq!(compute_cost_usd(&ep, 1000, 0, 0, 500), None);
    }

    #[test]
    fn compute_cost_usd_calculates_input_and_output() {
        let ep = ModelEndpoint {
            cost_per_1k_input_tokens: 3.0,
            cost_per_1k_output_tokens: 15.0,
            ..Default::default()
        };
        // 2k uncached input + 1k output = 6.0 + 15.0
        let cost = compute_cost_usd(&ep, 2000, 0, 0, 1000).unwrap();
        assert!((cost - 21.0).abs() < 1e-9);
    }

    #[test]
    fn compute_cost_usd_output_only_config() {
        let ep = ModelEndpoint {
            cost_per_1k_output_tokens: 10.0,
            ..Default::default()
        };
        // input rate zero -> only output counted
        let cost = compute_cost_usd(&ep, 5000, 0, 0, 200).unwrap();
        assert!((cost - 2.0).abs() < 1e-9);
    }

    #[test]
    fn compute_cost_usd_handles_fractional_tokens() {
        let ep = ModelEndpoint {
            cost_per_1k_input_tokens: 1.0,
            ..Default::default()
        };
        // 500 tokens -> 0.5
        let cost = compute_cost_usd(&ep, 500, 0, 0, 0).unwrap();
        assert!((cost - 0.5).abs() < 1e-9);
    }

    #[test]
    fn compute_cost_usd_uses_cache_lane_prices_when_configured() {
        let ep = ModelEndpoint {
            cost_per_1k_input_tokens: 3.0,
            cost_per_1k_output_tokens: 15.0,
            cost_per_1k_cache_read_tokens: Some(0.3),
            cost_per_1k_cache_write_tokens: Some(3.75),
            ..Default::default()
        };
        // 1k normal + 2k read + 1k write + 1k output = 3 + .6 + 3.75 + 15
        let cost = compute_cost_usd(&ep, 1000, 2000, 1000, 1000).unwrap();
        assert!((cost - 22.35).abs() < 1e-9);
    }

    #[test]
    fn compute_cost_usd_allows_cache_only_pricing() {
        let ep = ModelEndpoint {
            cost_per_1k_cache_read_tokens: Some(0.25),
            ..Default::default()
        };
        let cost = compute_cost_usd(&ep, 0, 1000, 0, 0).unwrap();
        assert!((cost - 0.25).abs() < 1e-9);
    }

    #[test]
    fn load_reads_existing_file() {
        let dir = std::env::temp_dir().join(format!("haven_test_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        let mut cfg = AppConfig::default();
        cfg.media.audio.max_duration_secs = 37;
        cfg.context_limits.response_token_floor = 48_000;
        cfg.context_limits.file_max_full_file_bytes = 256_000;
        cfg.context_limits.file_max_patch_bytes = 4 * 1024 * 1024;
        cfg.context_limits.file_line_max_bytes = 32_000;
        cfg.context_limits.file_byte_read_max_bytes = 8 * 1024 * 1024;
        cfg.context_limits.file_write_max_bytes = 32 * 1024 * 1024;
        std::fs::write(&path, toml::to_string_pretty(&cfg).unwrap()).unwrap();
        let loader = ConfigLoader::load_from(&path).unwrap();
        assert_eq!(loader.config().media.audio.max_duration_secs, 37);
        assert_eq!(loader.config().context_limits.response_token_floor, 48_000);
        assert_eq!(
            loader.config().context_limits.file_max_full_file_bytes,
            256_000
        );
        assert_eq!(
            loader.config().context_limits.file_max_patch_bytes,
            4 * 1024 * 1024
        );
        assert_eq!(loader.config().context_limits.file_line_max_bytes, 32_000);
        assert_eq!(
            loader.config().context_limits.file_byte_read_max_bytes,
            8 * 1024 * 1024
        );
        assert_eq!(
            loader.config().context_limits.file_write_max_bytes,
            32 * 1024 * 1024
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_migrates_legacy_builtin_permission_and_tool_settings_names() {
        let dir = std::env::temp_dir().join(format!("haven_test_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        let mut config = AppConfig::default();
        config.security.permissions = vec![
            StoredPermission {
                key: "haven.mcp.mcp_add".into(),
                effect: crate::types::PermissionEffect::Allow,
            },
            StoredPermission {
                key: "haven.mcp.add".into(),
                effect: crate::types::PermissionEffect::Deny,
            },
        ];
        config.tool_settings.insert(
            "media.volume_get".into(),
            ToolConfig {
                enabled: false,
                ..ToolConfig::default()
            },
        );
        config
            .tool_settings
            .insert("media.get_volume".into(), ToolConfig::default());
        std::fs::write(&path, toml::to_string_pretty(&config).unwrap()).unwrap();

        let loader = ConfigLoader::load_from(&path).unwrap();
        assert_eq!(
            loader.config().security.permissions,
            [StoredPermission {
                key: "haven.mcp.add".into(),
                effect: crate::types::PermissionEffect::Deny,
            }]
        );
        assert!(!loader.config().tool_settings["media.get_volume"].enabled);
        let persisted = std::fs::read_to_string(&path).unwrap();
        assert!(!persisted.contains("haven.mcp.mcp_add"));
        assert!(!persisted.contains("media.volume_get"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_backs_up_noncanonical_provider_api_style() {
        let dir = std::env::temp_dir().join(format!("haven_test_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        std::fs::write(
            &path,
            r#"
[[llm.providers]]
name = "deepseek"
provider = "deepseek"
api_style = "deepseek-responses"
base_url = "https://api.deepseek.com"
"#,
        )
        .unwrap();

        let loader = ConfigLoader::load_from(&path).unwrap();
        assert_eq!(loader.config(), &AppConfig::default());
        let backups = dir
            .read_dir()
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| {
                let name = entry.file_name().into_string().unwrap();
                name.starts_with("config.toml.") && name.ends_with(".bak")
            })
            .count();
        assert_eq!(backups, 1, "noncanonical wire styles must require reset");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_backs_up_unknown_media_and_web_search_modes() {
        let dir = std::env::temp_dir().join(format!("haven_test_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        let original = r#"
[media.ocr]
provider = "future-ocr"

[[llm.providers]]
name = "primary"
provider = "openai"
default_web_search = "alway"
"#;
        std::fs::write(&path, original).unwrap();

        let loader = ConfigLoader::load_from(&path).unwrap();

        assert_eq!(loader.config(), &AppConfig::default());
        let backups: Vec<_> = dir
            .read_dir()
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| {
                let name = entry.file_name().into_string().unwrap();
                name.starts_with("config.toml.") && name.ends_with(".bak")
            })
            .collect();
        assert_eq!(
            backups.len(),
            1,
            "invalid semantic config must be backed up"
        );
        assert_eq!(
            std::fs::read_to_string(backups[0].path()).unwrap(),
            original
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_rejects_old_fields_and_plaintext_credentials() {
        let cases = [
            ("removed root section", "[audio]\nmax_duration_secs = 20\n"),
            (
                "old retention location",
                "[memory]\nhistory_retention_days = 42\n",
            ),
            (
                "removed model role",
                "[llm]\nbalanced_model = \"old-model\"\n",
            ),
            (
                "old model provider reference field",
                "[[llm.models]]\nid = \"assistant\"\nprovider = \"primary\"\nmodel = \"gpt-test\"\n",
            ),
            (
                "legacy response token field name",
                "[context_limits]\nmax_response_tokens = 48000\n",
            ),
            (
                "plaintext provider credential",
                "[[llm.providers]]\nname = \"primary\"\napi_key = \"old-secret\"\n",
            ),
            (
                "missing explicit provider media capabilities",
                "[[llm.providers]]\nname = \"primary\"\nprovider = \"openai\"\nbase_url = \"https://api.openai.com/v1\"\n",
            ),
            (
                "plaintext OCR credential",
                "[media.ocr]\napi_key = \"old-secret\"\n",
            ),
            (
                "plaintext MCP environment",
                "[[mcp_servers]]\nname = \"example\"\nenv = [\"TOKEN=old-secret\"]\n",
            ),
            (
                "invalid MCP server name",
                "[[mcp_servers]]\nname = \"server.name\"\n",
            ),
            (
                "duplicate MCP server name",
                "[[mcp_servers]]\nname = \"server\"\n\n[[mcp_servers]]\nname = \"server\"\n",
            ),
            (
                "integer outside JavaScript safe range",
                "[context_limits]\nmax_upload_total_bytes = 9007199254740992\n",
            ),
            (
                "context ratio outside its declared range",
                "[context_limits]\ncompaction_ratio = 1.0\n",
            ),
            (
                "removed media provider credential",
                "[media.stt]\napi_key = \"old-secret\"\n",
            ),
        ];

        for (case, source) in cases {
            let dir = std::env::temp_dir().join(format!("haven_test_{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&dir).unwrap();
            let path = dir.join("config.toml");
            std::fs::write(&path, source).unwrap();

            let loader = ConfigLoader::load_from(&path).unwrap();
            assert_eq!(loader.config(), &AppConfig::default(), "{case}");
            let backup_count = dir
                .read_dir()
                .unwrap()
                .filter_map(Result::ok)
                .filter(|entry| {
                    entry.file_name().to_str().is_some_and(|name| {
                        name.starts_with("config.toml.") && name.ends_with(".bak")
                    })
                })
                .count();
            assert_eq!(
                backup_count, 1,
                "{case} must be preserved for manual recovery"
            );
            assert_eq!(std::fs::read_to_string(&path).unwrap(), source);
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    #[test]
    fn load_backs_up_unknown_security_fields() {
        let dir = std::env::temp_dir().join(format!("haven_test_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        std::fs::write(
            &path,
            r#"
[security]
confirmation_mode = "always"
"#,
        )
        .unwrap();

        let loader = ConfigLoader::load_from(&path).unwrap();
        assert_eq!(loader.config(), &AppConfig::default());
        let backups: Vec<_> = dir
            .read_dir()
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| {
                let name = entry.file_name().into_string().unwrap();
                name.starts_with("config.toml.") && name.ends_with(".bak")
            })
            .collect();
        assert_eq!(
            backups.len(),
            1,
            "unknown security fields must require reset"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_backs_up_unknown_media_fields() {
        let dir = std::env::temp_dir().join(format!("haven_test_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        std::fs::write(
            &path,
            r#"
[media.stt]
provider_name = "openai"
api_key = "old-secret"
base_url = "https://old.example/v1"

[media.tts]
provider_name = "openai"
api_key = "old-secret"
"#,
        )
        .unwrap();

        let loader = ConfigLoader::load_from(&path).unwrap();
        assert_eq!(loader.config(), &AppConfig::default());
        let backups = dir
            .read_dir()
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| {
                let name = entry.file_name().into_string().unwrap();
                name.starts_with("config.toml.") && name.ends_with(".bak")
            })
            .count();
        assert_eq!(backups, 1, "removed media credentials must require reset");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_backs_up_legacy_media_provider_keys() {
        for legacy_config in [
            "[media.stt]\nprovider = \"llm\"\n",
            "[media.tts]\nprovider = \"none\"\n",
            "[media.image_gen]\nprovider = \"none\"\n",
        ] {
            let dir = std::env::temp_dir().join(format!("haven_test_{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&dir).unwrap();
            let path = dir.join("config.toml");
            std::fs::write(&path, legacy_config).unwrap();

            let loader = ConfigLoader::load_from(&path).unwrap();
            assert_eq!(loader.config(), &AppConfig::default());
            let backups = dir
                .read_dir()
                .unwrap()
                .filter_map(Result::ok)
                .filter(|entry| {
                    let name = entry.file_name().into_string().unwrap();
                    name.starts_with("config.toml.") && name.ends_with(".bak")
                })
                .count();
            assert_eq!(backups, 1, "legacy media provider key must be backed up");
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    #[test]
    fn load_backs_up_unknown_media_provider_reference() {
        let dir = std::env::temp_dir().join(format!("haven_test_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        std::fs::write(
            &path,
            r#"
[media.stt]
provider_name = "deepgram"
model = "nova-3"
"#,
        )
        .unwrap();

        let loader = ConfigLoader::load_from(&path).unwrap();
        assert_eq!(loader.config(), &AppConfig::default());
        let backups = dir
            .read_dir()
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| {
                let name = entry.file_name().into_string().unwrap();
                name.starts_with("config.toml.") && name.ends_with(".bak")
            })
            .count();
        assert_eq!(
            backups, 1,
            "unknown media provider references must require reset"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_accepts_media_provider_connection_name_distinct_from_vendor() {
        let dir = std::env::temp_dir().join(format!("haven_test_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        std::fs::write(
            &path,
            r#"
[media.stt]
provider_name = "deepgram-voice"

[media.tts]
provider_name = "speech-connection"

[media.image_gen]
provider_name = "image-connection"

[[llm.providers]]
name = "deepgram-voice"
provider = "deepgram"
base_url = "https://api.deepgram.com"
media_capabilities = { stt = false, tts = false, image_gen = false }

[[llm.providers]]
name = "speech-connection"
provider = "openai"
base_url = "https://speech.example/v1"
media_capabilities = { stt = false, tts = false, image_gen = false }

[[llm.providers]]
name = "image-connection"
provider = "openai"
base_url = "https://image.example/v1"
media_capabilities = { stt = false, tts = false, image_gen = false }

[[llm.models]]
id = "transcription-model"
provider_name = "deepgram-voice"
model = "nova-3"
"#,
        )
        .unwrap();

        let loader = ConfigLoader::load_from(&path).unwrap();
        assert_eq!(loader.config().media.stt.provider_name, "deepgram-voice");
        assert_eq!(loader.config().media.tts.provider_name, "speech-connection");
        assert_eq!(
            loader.config().media.image_gen.provider_name,
            "image-connection"
        );
        assert_eq!(loader.config().llm.providers[0].name, "deepgram-voice");
        assert_eq!(loader.config().llm.providers[0].provider, "deepgram");
        assert_eq!(
            loader.config().llm.models[0].provider_name,
            "deepgram-voice"
        );
        let backups = dir
            .read_dir()
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| {
                let name = entry.file_name().into_string().unwrap();
                name.starts_with("config.toml.") && name.ends_with(".bak")
            })
            .count();
        assert_eq!(backups, 0, "named provider references are current config");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_backs_up_unparsable_config_instead_of_destroying_it() {
        let dir = std::env::temp_dir().join(format!("haven_test_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        // Genuinely unparsable TOML (unterminated array).
        let original = "media = { audio = { max_duration_secs = [1, 2\n";
        std::fs::write(&path, original).unwrap();

        // The corrupt file must NOT be silently discarded: it is backed up
        // (timestamped .bak), defaults remain available to runtime readers,
        // and the loader records that writes must be refused.
        let loader = ConfigLoader::load_from(&path).unwrap();
        assert!(loader.recovery_required);
        assert!(loader.ensure_writable().is_err());
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
    fn load_rejects_oversized_config_without_backup_or_default_fallback() {
        let dir = std::env::temp_dir().join(format!("haven_test_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        let original = vec![b'x'; MAX_CONFIG_FILE_BYTES + 1];
        std::fs::write(&path, &original).unwrap();

        let error = ConfigLoader::load_from(&path).unwrap_err();

        assert!(error.to_string().contains("exceeds the 4194304-byte limit"));
        assert_eq!(std::fs::read(&path).unwrap(), original);
        let backups = dir
            .read_dir()
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| {
                let name = entry.file_name().to_string_lossy().into_owned();
                name.starts_with("config.toml.") && name.ends_with(".bak")
            })
            .count();
        assert_eq!(backups, 0, "oversized config must not be copied to backup");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_rejects_removed_skill_resource_limits_and_backs_up_config() {
        let dir = std::env::temp_dir().join(format!("haven_test_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        let original =
            "[skills_exec]\nwork_dir = \"custom\"\ncpu_time_secs = 30\nmax_memory_mb = 512\n";
        std::fs::write(&path, original).unwrap();

        let loader = ConfigLoader::load_from(&path).unwrap();

        assert_eq!(loader.config().skills_exec, SkillsExecConfig::default());
        assert!(loader.recovery_required);
        assert!(loader.ensure_writable().is_err());
        let backups: Vec<_> = dir
            .read_dir()
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| {
                let name = entry.file_name().into_string().unwrap();
                name.starts_with("config.toml.") && name.ends_with(".bak")
            })
            .collect();
        assert_eq!(backups.len(), 1, "removed config fields must be backed up");
        assert_eq!(
            std::fs::read_to_string(backups[0].path()).unwrap(),
            original
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn automatic_backup_retention_keeps_newest_and_ignores_manual_backups() {
        let dir = std::env::temp_dir().join(format!("haven_test_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        for timestamp in 1..=12 {
            std::fs::write(
                dir.join(format!("config.toml.{timestamp}.1.0.bak")),
                timestamp.to_string(),
            )
            .unwrap();
        }
        let manual = dir.join("config.toml.manual.bak");
        std::fs::write(&manual, "manual recovery copy").unwrap();

        prune_timestamped_backups(&path);

        let automatic: Vec<_> = dir
            .read_dir()
            .unwrap()
            .filter_map(Result::ok)
            .filter_map(|entry| parse_automatic_backup_identity(&entry.path(), &path))
            .collect();
        assert_eq!(automatic.len(), MAX_AUTOMATIC_CONFIG_BACKUPS);
        assert!(dir.join("config.toml.12.1.0.bak").exists());
        assert!(dir.join("config.toml.3.1.0.bak").exists());
        assert!(!dir.join("config.toml.2.1.0.bak").exists());
        assert!(manual.exists(), "manual backup must be preserved");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_backs_up_config_with_duplicate_provider_names() {
        let dir = std::env::temp_dir().join(format!("haven_test_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        let original =
            "[[llm.providers]]\nname = \"primary\"\n\n[[llm.providers]]\nname = \"primary\"\n";
        std::fs::write(&path, original).unwrap();

        let loader = ConfigLoader::load_from(&path).unwrap();

        assert!(loader.config().llm.providers.is_empty());
        let backups: Vec<_> = dir
            .read_dir()
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| {
                let name = entry.file_name().into_string().unwrap();
                name.starts_with("config.toml.") && name.ends_with(".bak")
            })
            .collect();
        assert_eq!(backups.len(), 1, "invalid config must be backed up");
        assert_eq!(
            std::fs::read_to_string(backups[0].path()).unwrap(),
            original
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_backs_up_config_with_media_timeout_outside_settings_range() {
        let dir = std::env::temp_dir().join(format!("haven_test_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        let original = "[media.stt]\ntimeout_secs = 0\n";
        std::fs::write(&path, original).unwrap();

        let loader = ConfigLoader::load_from(&path).unwrap();

        assert_eq!(loader.config().media.stt.timeout_secs, 30);
        let backups: Vec<_> = dir
            .read_dir()
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| {
                let name = entry.file_name().into_string().unwrap();
                name.starts_with("config.toml.") && name.ends_with(".bak")
            })
            .collect();
        assert_eq!(backups.len(), 1, "invalid config must be backed up");
        assert_eq!(
            std::fs::read_to_string(backups[0].path()).unwrap(),
            original
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_caps_oversized_known_fact_prompt_setting_without_discarding_config() {
        let dir = std::env::temp_dir().join(format!("haven_test_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        let original = "[context_limits]\nmax_known_facts = 1000\n";
        std::fs::write(&path, original).unwrap();

        let loader = ConfigLoader::load_from(&path).unwrap();

        assert_eq!(
            loader.config().context_limits.max_known_facts,
            MAX_FACTS_IN_EXTRACTION_PROMPT
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
        let backups = dir
            .read_dir()
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| {
                let name = entry.file_name().into_string().unwrap();
                name.starts_with("config.toml.") && name.ends_with(".bak")
            })
            .count();
        assert_eq!(
            backups, 0,
            "a cap adjustment must not discard other settings"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn persist_writes_changes() {
        let dir = std::env::temp_dir().join(format!("haven_test_{}", uuid::Uuid::new_v4()));
        let path = dir.join("config.toml");
        let mut loader = ConfigLoader::load_from(&path).unwrap();
        loader.config_mut().session.max_concurrent = 7;
        loader.persist().unwrap();
        let reloaded = ConfigLoader::load_from(&path).unwrap();
        assert_eq!(reloaded.config().session.max_concurrent, 7);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn persist_rejects_the_same_semantic_contract_as_load() {
        let dir = std::env::temp_dir().join(format!("haven_test_{}", uuid::Uuid::new_v4()));
        let path = dir.join("config.toml");
        let mut loader = ConfigLoader::load_from(&path).unwrap();
        let original = std::fs::read_to_string(&path).unwrap();
        loader.config_mut().llm.providers.push(ProviderConfig {
            name: "primary".into(),
            api_style: Some("unknown-style".into()),
            ..Default::default()
        });

        assert!(loader.persist().is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn persist_rejects_oversized_serialized_config_without_replacing_source() {
        let dir = std::env::temp_dir().join(format!("haven_test_{}", uuid::Uuid::new_v4()));
        let path = dir.join("config.toml");
        let mut loader = ConfigLoader::load_from(&path).unwrap();
        let original = std::fs::read(&path).unwrap();
        loader.config_mut().llm.providers.push(ProviderConfig {
            name: "primary".into(),
            no_proxy: Some("x".repeat(MAX_CONFIG_FILE_BYTES + 1)),
            ..Default::default()
        });

        let error = loader.persist().unwrap_err();

        assert!(error.to_string().contains("serialized config exceeds"));
        assert_eq!(std::fs::read(&path).unwrap(), original);
        assert_eq!(
            dir.read_dir().unwrap().count(),
            1,
            "oversized config must be rejected before creating a temp file"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn persist_checks_external_source_fingerprint_with_a_bounded_read() {
        let dir = std::env::temp_dir().join(format!("haven_test_{}", uuid::Uuid::new_v4()));
        let path = dir.join("config.toml");
        let mut loader = ConfigLoader::load_from(&path).unwrap();
        loader.config_mut().session.max_concurrent += 1;
        let external = vec![b'x'; MAX_CONFIG_FILE_BYTES + 1];
        std::fs::write(&path, &external).unwrap();

        let error = loader.persist().unwrap_err();

        assert!(error.to_string().contains("exceeds the 4194304-byte limit"));
        assert_eq!(std::fs::read(&path).unwrap(), external);
        assert_eq!(
            dir.read_dir().unwrap().count(),
            1,
            "failed fingerprint read must remove the staged config"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
