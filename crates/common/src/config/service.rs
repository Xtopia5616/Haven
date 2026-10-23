//! Versioned configuration snapshots and serialized persistence.
//!
//! `ConfigLoader` remains the TOML codec and file-format boundary. This module
//! owns the live configuration snapshot, mutation serialization, versioning,
//! typed patches, and change notifications so callers do not coordinate a
//! shared loader mutex themselves.

#[cfg(test)]
use super::credentials::UnavailableCredentialStore;
use super::credentials::{
    CredentialSlot, OcrCredentialField, credential_references, hydrate_from_store,
    prepare_after_edit,
};
use super::{
    AppConfig, ConfigLoader, CredentialStore, LlmConfig, LogLevel, McpDiscoveryConfig,
    McpServerConfig, ModelConfig, SecurityConfig, SettingsPatch, SettingsSnapshot, SkillsConfig,
    SkillsExecConfig, ToolConfig,
};
use crate::types::ShellChoice;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::sync::{Mutex, PoisonError};

const CONFIG_CHANGE_NOTIFICATION_CAPACITY: usize = 64;

/// Monotonic in-process version of the live configuration snapshot.
pub type ConfigVersion = u64;

/// The top-level configuration sections that can invalidate runtime
/// consumers. This is intentionally coarse: consumers can compare the
/// immutable snapshot when they need field-level detail.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfigDomain {
    DefaultShell,
    Llm,
    Hotkey,
    Session,
    ContextLimits,
    Memory,
    Security,
    Media,
    Skills,
    SkillsExec,
    McpDiscovery,
    McpServers,
    Notification,
    Log,
    Tools,
    Hooks,
}

/// Notification emitted after a durable configuration mutation succeeds.
/// It contains no configuration values or secrets; consumers read the
/// corresponding immutable snapshot from `ConfigService`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfigChanged {
    pub version: ConfigVersion,
    pub domains: Vec<ConfigDomain>,
}

/// An immutable point-in-time view of the live configuration.
#[derive(Debug, Clone, PartialEq)]
pub struct ConfigSnapshot {
    pub version: ConfigVersion,
    pub config: AppConfig,
}

/// Result of a mutation. `change` is `None` for a no-op, so callers do not
/// rebuild runtime components or emit notifications unnecessarily.
#[derive(Debug)]
pub struct ConfigUpdate<T> {
    pub value: T,
    pub snapshot: ConfigSnapshot,
    pub change: Option<ConfigChanged>,
}

/// Typed mutations currently used by the settings and model commands.
#[derive(Debug, Clone)]
pub enum ConfigPatch {
    Settings(Box<SettingsPatch>),
    Llm(LlmConfig),
    LlmModel {
        model_config_id: String,
        patch: LlmModelPatch,
    },
    DefaultShell(ShellChoice),
    Security(SecurityConfig),
    SecurityPermissions(Vec<super::StoredPermission>),
    Media(Box<super::MediaConfig>),
    Skills {
        config: SkillsConfig,
        exec: SkillsExecConfig,
    },
    McpDiscovery(McpDiscoveryConfig),
    McpServers(Vec<McpServerConfig>),
    Tools(HashMap<String, ToolConfig>),
    LifecycleHooks(super::LifecycleHooksConfig),
    TrustHookWorkspace(std::path::PathBuf),
    UntrustHookWorkspace(std::path::PathBuf),
    LogLevel(LogLevel),
}

/// Field-level patch for a configured named model.
#[derive(Debug, Clone)]
pub enum LlmModelPatch {
    Replace(Box<ModelConfig>),
    Model(String),
    ReasoningEffort(Option<String>),
    WebSearch(Option<String>),
}

impl ConfigPatch {
    fn apply(self, config: &mut AppConfig) -> anyhow::Result<()> {
        match self {
            Self::Settings(settings) => config.apply_settings_patch(&settings),
            Self::Llm(llm) => config.llm = llm,
            Self::LlmModel {
                model_config_id,
                patch,
            } => {
                let model_config =
                    config
                        .llm
                        .model_config_mut(&model_config_id)
                        .ok_or_else(|| {
                            anyhow::anyhow!(
                                "unknown or unconfigured model configuration: {}",
                                model_config_id
                            )
                        })?;
                match patch {
                    LlmModelPatch::Replace(updated) => {
                        let mut updated = *updated;
                        updated.stamp_model_config_id(&model_config_id);
                        *model_config = updated;
                    }
                    LlmModelPatch::Model(provider_model_name) => {
                        model_config.model = provider_model_name
                    }
                    LlmModelPatch::ReasoningEffort(effort) => {
                        model_config.reasoning_effort = effort
                    }
                    LlmModelPatch::WebSearch(mode) => model_config.web_search = mode,
                }
            }
            Self::DefaultShell(shell) => config.default_shell = shell,
            Self::Security(security) => config.security = security,
            Self::SecurityPermissions(permissions) => config.security.permissions = permissions,
            Self::Media(media) => config.media = *media,
            Self::Skills {
                config: skills,
                exec,
            } => {
                config.skills = skills;
                config.skills_exec = exec;
            }
            Self::McpDiscovery(discovery) => config.mcp_discovery = discovery,
            Self::McpServers(servers) => config.mcp_servers = servers,
            Self::Tools(tool_settings) => config.tool_settings = tool_settings,
            Self::LifecycleHooks(hooks) => config.hooks = hooks,
            Self::TrustHookWorkspace(path) => {
                if !config
                    .trusted_hook_workspaces
                    .iter()
                    .any(|trusted| same_workspace_path(trusted, &path))
                {
                    config.trusted_hook_workspaces.push(path);
                }
            }
            Self::UntrustHookWorkspace(path) => {
                let old_len = config.trusted_hook_workspaces.len();
                config
                    .trusted_hook_workspaces
                    .retain(|trusted| !same_workspace_path(trusted, &path));
                anyhow::ensure!(
                    old_len != config.trusted_hook_workspaces.len(),
                    "trusted Hook workspace was not found"
                );
            }
            Self::LogLevel(level) => config.log.level = level,
        }
        Ok(())
    }
}

fn same_workspace_path(left: &std::path::Path, right: &std::path::Path) -> bool {
    #[cfg(windows)]
    {
        left.to_string_lossy()
            .eq_ignore_ascii_case(&right.to_string_lossy())
    }
    #[cfg(not(windows))]
    {
        left == right
    }
}

struct ConfigState {
    loader: ConfigLoader,
    version: ConfigVersion,
    credential_store: std::sync::Arc<dyn CredentialStore>,
    staged_credentials: HashMap<CredentialSlot, String>,
}

/// The single live configuration owner for an application process.
pub struct ConfigService {
    state: Mutex<ConfigState>,
    subscribers: Mutex<Vec<SyncSender<ConfigChanged>>>,
}

impl std::fmt::Debug for ConfigService {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ConfigService")
            .finish_non_exhaustive()
    }
}

impl ConfigService {
    /// Fail-closed constructor for callers without a persistent platform
    /// credential adapter. Configurations without credential references can
    /// load; configured references and credential writes return an error.
    /// Tests that need credentials must inject an explicit test store.
    #[cfg(test)]
    fn new(loader: ConfigLoader) -> anyhow::Result<Self> {
        Self::new_with_credential_store(loader, std::sync::Arc::new(UnavailableCredentialStore))
    }

    /// Create the live config owner with an explicit secure-store backend.
    pub fn new_with_credential_store(
        mut loader: ConfigLoader,
        credential_store: std::sync::Arc<dyn CredentialStore>,
    ) -> anyhow::Result<Self> {
        let mut config = loader.config().clone();
        super::validate_app_config(&config)?;
        hydrate_from_store(&mut config, credential_store.as_ref())?;
        *loader.config_mut() = config;
        Ok(Self {
            state: Mutex::new(ConfigState {
                loader,
                version: 0,
                credential_store,
                staged_credentials: HashMap::new(),
            }),
            subscribers: Mutex::new(Vec::new()),
        })
    }

    /// Stage an API-key update without exposing the secret through the
    /// SettingsSnapshot payload. The returned opaque reference may be sent back with
    /// SettingsSnapshot; the actual value remains in the credential store.
    pub fn stage_provider_credential(
        &self,
        provider_name: &str,
        value: &str,
    ) -> anyhow::Result<String> {
        if provider_name.trim().is_empty() || value.is_empty() {
            anyhow::bail!("provider name and API key are required");
        }
        self.stage_credential(
            CredentialSlot::ProviderApiKey(provider_name.to_string()),
            value,
        )
    }

    /// Stage the protected ChatGPT plan OAuth record for a dedicated provider.
    /// The serialized token bundle is kept in the secure store and only its
    /// opaque `cred-*` reference is returned to the settings draft.
    pub fn stage_chatgpt_plan_credential(
        &self,
        provider_name: &str,
        value: &str,
    ) -> anyhow::Result<String> {
        if provider_name.trim().is_empty() || value.is_empty() {
            anyhow::bail!("provider name and ChatGPT plan credential are required");
        }
        self.stage_credential(
            CredentialSlot::ProviderChatGptPlan(provider_name.to_string()),
            value,
        )
    }

    /// Remove one exact staged ChatGPT credential if its OAuth result was
    /// abandoned before the Settings draft adopted the returned reference.
    pub fn discard_staged_chatgpt_plan_credential(&self, reference: &str) -> anyhow::Result<()> {
        crate::config::validate_credential_reference(reference)?;
        let mut state = self.lock_state_mut()?;
        let active_owner = state.loader.config().llm.providers.iter().any(|provider| {
            super::is_chatgpt_plan_provider(&provider.provider)
                && provider.api_key_ref.as_deref() == Some(reference)
        });
        if active_owner {
            return Ok(());
        }
        let slot = state
            .staged_credentials
            .iter()
            .find_map(|(slot, staged_reference)| {
                (matches!(slot, CredentialSlot::ProviderChatGptPlan(_))
                    && staged_reference == reference)
                    .then(|| slot.clone())
            });
        let Some(slot) = slot else {
            return Ok(());
        };
        state
            .credential_store
            .delete(reference)
            .map_err(|_| anyhow::anyhow!("failed to remove abandoned ChatGPT plan credential"))?;
        state.staged_credentials.remove(&slot);
        Ok(())
    }

    /// Move one staged ChatGPT credential to the provider name selected by the
    /// current Settings draft, without exposing or rewriting its secret.
    pub fn reassign_staged_chatgpt_plan_credential(
        &self,
        reference: &str,
        provider_name: &str,
    ) -> anyhow::Result<()> {
        crate::config::validate_credential_reference(reference)?;
        if provider_name.trim().is_empty() {
            anyhow::bail!("provider name is required");
        }
        let mut state = self.lock_state_mut()?;
        let source_slot = state
            .staged_credentials
            .iter()
            .find_map(|(slot, staged_reference)| {
                (matches!(slot, CredentialSlot::ProviderChatGptPlan(_))
                    && staged_reference == reference)
                    .then(|| slot.clone())
            });
        let Some(source_slot) = source_slot else {
            let active_owner = state.loader.config().llm.providers.iter().any(|provider| {
                super::is_chatgpt_plan_provider(&provider.provider)
                    && provider.api_key_ref.as_deref() == Some(reference)
            });
            anyhow::ensure!(
                active_owner,
                "ChatGPT plan credential is not staged or active"
            );
            return Ok(());
        };
        let target_slot = CredentialSlot::ProviderChatGptPlan(provider_name.to_string());
        if source_slot == target_slot {
            return Ok(());
        }
        if state.loader.config().llm.providers.iter().any(|provider| {
            provider.name == provider_name && !super::is_chatgpt_plan_provider(&provider.provider)
        }) {
            anyhow::bail!("ChatGPT credential cannot be assigned to another provider type");
        }
        anyhow::ensure!(
            !state.staged_credentials.contains_key(&target_slot),
            "another ChatGPT credential is already staged for this provider"
        );
        state.staged_credentials.remove(&source_slot);
        state
            .staged_credentials
            .insert(target_slot, reference.to_string());
        Ok(())
    }

    /// Read a ChatGPT plan token bundle only when its credential reference is
    /// owned by an active or staged `chatgpt-plan` provider.
    pub fn read_chatgpt_plan_credential(&self, reference: &str) -> anyhow::Result<String> {
        crate::config::validate_credential_reference(reference)?;
        let state = self.lock_state()?;
        let active_owner = state.loader.config().llm.providers.iter().any(|provider| {
            super::is_chatgpt_plan_provider(&provider.provider)
                && provider.api_key_ref.as_deref() == Some(reference)
        });
        let staged_owner = state
            .staged_credentials
            .iter()
            .any(|(slot, staged_reference)| {
                matches!(slot, CredentialSlot::ProviderChatGptPlan(_))
                    && staged_reference == reference
            });
        anyhow::ensure!(
            active_owner || staged_owner,
            "ChatGPT plan credential is not configured"
        );
        state
            .credential_store
            .read(reference)
            .map_err(|_| anyhow::anyhow!("failed to read ChatGPT plan credential securely"))?
            .ok_or_else(|| {
                anyhow::anyhow!("ChatGPT plan credential is missing from secure storage")
            })
    }

    /// Resolve a named ChatGPT plan provider from the active configuration or
    /// an unsaved OAuth login staged for that provider.
    pub fn chatgpt_plan_credential_reference(
        &self,
        provider_name: &str,
    ) -> anyhow::Result<Option<String>> {
        let state = self.lock_state()?;
        if let Some(reference) = state
            .staged_credentials
            .get(&CredentialSlot::ProviderChatGptPlan(
                provider_name.to_string(),
            ))
        {
            return Ok(Some(reference.clone()));
        }
        if let Some(provider) = state
            .loader
            .config()
            .llm
            .provider_config_by_name(provider_name)
        {
            if !super::is_chatgpt_plan_provider(&provider.provider) {
                return Ok(None);
            }
            if provider.api_key_ref.is_some() {
                return Ok(provider.api_key_ref.clone());
            }
        }
        Ok(None)
    }

    /// Replace a ChatGPT plan token bundle in place after OAuth refresh. The
    /// existing reference remains stable while access and rotating refresh
    /// tokens are replaced atomically by the secure-store adapter.
    pub fn replace_chatgpt_plan_credential(
        &self,
        reference: &str,
        value: &str,
    ) -> anyhow::Result<()> {
        crate::config::validate_credential_reference(reference)?;
        anyhow::ensure!(!value.is_empty(), "ChatGPT plan credential cannot be empty");
        let state = self.lock_state()?;
        let active_owner = state.loader.config().llm.providers.iter().any(|provider| {
            super::is_chatgpt_plan_provider(&provider.provider)
                && provider.api_key_ref.as_deref() == Some(reference)
        });
        let staged_owner = state
            .staged_credentials
            .iter()
            .any(|(slot, staged_reference)| {
                matches!(slot, CredentialSlot::ProviderChatGptPlan(_))
                    && staged_reference == reference
            });
        anyhow::ensure!(
            active_owner || staged_owner,
            "ChatGPT plan credential is not configured"
        );
        state
            .credential_store
            .write(reference, value)
            .map_err(|_| anyhow::anyhow!("failed to update ChatGPT plan credential securely"))
    }

    /// Stage a dedicated AI Search API key for one model's Alibaba search config.
    pub fn stage_independent_search_credential(
        &self,
        model_id: &str,
        value: &str,
    ) -> anyhow::Result<String> {
        if model_id.trim().is_empty() || value.is_empty() {
            anyhow::bail!("model id and AI Search API key are required");
        }
        self.stage_credential(
            CredentialSlot::IndependentSearchApiKey(model_id.to_string()),
            value,
        )
    }

    /// Stage one of the dedicated OCR credentials.
    pub fn stage_ocr_credential(
        &self,
        field: OcrCredentialField,
        value: &str,
    ) -> anyhow::Result<String> {
        if value.is_empty() {
            anyhow::bail!("OCR credential cannot be empty");
        }
        self.stage_credential(
            match field {
                OcrCredentialField::ApiKey => CredentialSlot::OcrApiKey,
                OcrCredentialField::ApiSecret => CredentialSlot::OcrApiSecret,
            },
            value,
        )
    }

    fn stage_credential(&self, slot: CredentialSlot, value: &str) -> anyhow::Result<String> {
        let mut state = self.lock_state_mut()?;
        state.loader.ensure_writable()?;
        let reference = crate::types::new_id("cred");
        state
            .credential_store
            .write(&reference, value)
            .map_err(|_| anyhow::anyhow!("failed to write credential to secure storage"))?;
        if let Some(previous) = state.staged_credentials.insert(slot, reference.clone())
            && let Err(error) = state.credential_store.delete(&previous)
        {
            tracing::warn!(
                error = %crate::error::sanitize_error_text(&error.to_string()),
                "failed to remove superseded staged credential"
            );
        }
        Ok(reference)
    }

    /// Remove staged values if a user discards an unsaved SettingsSnapshot edit.
    pub fn discard_staged_credentials(&self) -> anyhow::Result<()> {
        let mut state = self.lock_state_mut()?;
        let staged = std::mem::take(&mut state.staged_credentials);
        let mut first_error = None;
        for reference in staged.into_values() {
            if let Err(error) = state.credential_store.delete(&reference)
                && first_error.is_none()
            {
                first_error = Some(error);
            }
        }
        if let Some(error) = first_error {
            return Err(anyhow::anyhow!(
                "failed to remove an unsaved credential from secure storage: {}",
                crate::error::sanitize_error_text(&error.to_string())
            ));
        }
        Ok(())
    }

    pub fn snapshot(&self) -> anyhow::Result<ConfigSnapshot> {
        let state = self.lock_state()?;
        Ok(ConfigSnapshot {
            version: state.version,
            config: state.loader.config().clone(),
        })
    }

    pub fn settings(&self) -> anyhow::Result<SettingsSnapshot> {
        let state = self.lock_state()?;
        state.loader.ensure_writable()?;
        Ok(SettingsSnapshot::from(state.loader.config()))
    }

    pub fn path(&self) -> anyhow::Result<std::path::PathBuf> {
        Ok(self.lock_state()?.loader.path().to_path_buf())
    }

    /// Subscribe to successful durable mutations using a bounded,
    /// non-blocking invalidation channel. Notifications identify changed
    /// domains and versions; consumers should read the authoritative config
    /// from [`Self::snapshot`] instead of treating notifications as state.
    /// A subscriber that does not keep up is disconnected after the bounded
    /// queue fills. It can drain notifications already queued before observing
    /// the disconnect, then resubscribe and refresh its snapshot.
    pub fn subscribe(&self) -> anyhow::Result<Receiver<ConfigChanged>> {
        let (sender, receiver) = mpsc::sync_channel(CONFIG_CHANGE_NOTIFICATION_CAPACITY);
        self.subscribers.lock().map_err(lock_error)?.push(sender);
        Ok(receiver)
    }

    /// Apply one typed patch, persist it atomically, advance the version and
    /// publish a secret-free change notification.
    pub fn apply_patch(&self, patch: ConfigPatch) -> anyhow::Result<ConfigUpdate<()>> {
        self.edit(|config| patch.apply(config))
    }

    /// Apply a typed mutation while the serialized config state is locked; a
    /// failed save restores the previous in-memory snapshot.
    pub fn edit<T>(
        &self,
        edit: impl FnOnce(&mut AppConfig) -> anyhow::Result<T>,
    ) -> anyhow::Result<ConfigUpdate<T>> {
        let (update, change) = {
            let mut state = self.lock_state_mut()?;
            state.loader.ensure_writable()?;
            let before = state.loader.config().clone();
            let value = match edit(state.loader.config_mut()) {
                Ok(value) => value,
                Err(error) => {
                    *state.loader.config_mut() = before;
                    return Err(error);
                }
            };
            if let Err(error) = super::validate_app_config(state.loader.config()) {
                *state.loader.config_mut() = before;
                return Err(error);
            }
            let prepared = {
                let ConfigState {
                    loader,
                    credential_store,
                    staged_credentials,
                    ..
                } = &mut *state;
                prepare_after_edit(
                    &before,
                    loader.config_mut(),
                    credential_store.as_ref(),
                    staged_credentials,
                )
            };
            let consumed_staged = match prepared {
                Ok(consumed) => consumed,
                Err(error) => {
                    *state.loader.config_mut() = before;
                    return Err(error);
                }
            };
            let after = state.loader.config().clone();

            if before == after {
                let snapshot = ConfigSnapshot {
                    version: state.version,
                    config: after,
                };
                (
                    ConfigUpdate {
                        value,
                        snapshot,
                        change: None,
                    },
                    None,
                )
            } else {
                if let Err(error) = state.loader.persist() {
                    *state.loader.config_mut() = before;
                    return Err(error);
                }
                for slot in consumed_staged {
                    state.staged_credentials.remove(&slot);
                }
                let active_references = credential_references(&after);
                for stale in credential_references(&before).difference(&active_references) {
                    if let Err(error) = state.credential_store.delete(stale) {
                        tracing::warn!(
                            error = %crate::error::sanitize_error_text(&error.to_string()),
                            "failed to remove a superseded credential"
                        );
                    }
                }
                state.version = state.version.saturating_add(1);
                let change = ConfigChanged {
                    version: state.version,
                    domains: changed_domains(&before, &after),
                };
                let snapshot = ConfigSnapshot {
                    version: state.version,
                    config: after,
                };
                let update = ConfigUpdate {
                    value,
                    snapshot,
                    change: Some(change.clone()),
                };
                (update, Some(change))
            }
        };

        if let Some(change) = change {
            self.publish(change);
        }
        Ok(update)
    }

    fn publish(&self, change: ConfigChanged) {
        let Ok(mut subscribers) = self.subscribers.lock() else {
            tracing::warn!("config change notification lock poisoned");
            return;
        };
        subscribers.retain(|subscriber| match subscriber.try_send(change.clone()) {
            Ok(()) => true,
            Err(mpsc::TrySendError::Full(_) | mpsc::TrySendError::Disconnected(_)) => false,
        });
    }

    fn lock_state(&self) -> anyhow::Result<std::sync::MutexGuard<'_, ConfigState>> {
        self.state.lock().map_err(lock_error)
    }

    fn lock_state_mut(&self) -> anyhow::Result<std::sync::MutexGuard<'_, ConfigState>> {
        self.state.lock().map_err(lock_error)
    }
}

fn lock_error<T>(error: PoisonError<T>) -> anyhow::Error {
    anyhow::anyhow!("configuration state lock poisoned: {error}")
}

fn changed_domains(before: &AppConfig, after: &AppConfig) -> Vec<ConfigDomain> {
    let mut domains = Vec::new();
    macro_rules! changed {
        ($domain:ident, $field:ident) => {
            if before.$field != after.$field {
                domains.push(ConfigDomain::$domain);
            }
        };
    }
    changed!(DefaultShell, default_shell);
    changed!(Llm, llm);
    changed!(Hotkey, hotkey);
    changed!(Session, session);
    changed!(ContextLimits, context_limits);
    changed!(Memory, memory);
    changed!(Security, security);
    changed!(Media, media);
    changed!(Skills, skills);
    changed!(SkillsExec, skills_exec);
    changed!(McpDiscovery, mcp_discovery);
    changed!(McpServers, mcp_servers);
    changed!(Notification, notification);
    changed!(Log, log);
    changed!(Tools, tool_settings);
    if before.hooks != after.hooks
        || before.trusted_hook_workspaces != after.trusted_hook_workspaces
    {
        domains.push(ConfigDomain::Hooks);
    }
    domains
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{CredentialStore, InMemoryCredentialStore};
    use tempfile::tempdir;

    fn service() -> (ConfigService, tempfile::TempDir) {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let loader = ConfigLoader::load_from(&path).unwrap();
        (
            ConfigService::new_with_credential_store(
                loader,
                std::sync::Arc::new(InMemoryCredentialStore::default()),
            )
            .unwrap(),
            dir,
        )
    }

    #[test]
    fn invalid_config_fallback_remains_read_only_until_restart_recovery() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let original = "media = { audio = { max_duration_secs = [1, 2\n";
        std::fs::write(&path, original).unwrap();
        let loader = ConfigLoader::load_from(&path).unwrap();
        let service = ConfigService::new_with_credential_store(
            loader,
            std::sync::Arc::new(InMemoryCredentialStore::default()),
        )
        .unwrap();

        assert_eq!(service.snapshot().unwrap().config, AppConfig::default());
        let settings_error = service.settings().unwrap_err();
        assert!(settings_error.to_string().contains("只读恢复"));

        let edit_error = service
            .edit(|config| {
                config.session.max_steps_per_run += 1;
                Ok(())
            })
            .unwrap_err();
        assert!(edit_error.to_string().contains("只读恢复"));
        let credential_error = service
            .stage_provider_credential("primary", "secret")
            .unwrap_err();
        assert!(credential_error.to_string().contains("只读恢复"));

        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
    }

    #[test]
    fn default_constructor_fails_closed_for_credential_references_and_writes() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let mut loader = ConfigLoader::load_from(&path).unwrap();
        loader
            .config_mut()
            .llm
            .providers
            .push(crate::config::ProviderConfig {
                name: "primary".into(),
                api_key_ref: Some(crate::types::new_id("cred")),
                ..Default::default()
            });
        assert!(ConfigService::new(loader).is_err());

        let loader = ConfigLoader::load_from(&path).unwrap();
        let before = std::fs::read_to_string(&path).unwrap();
        let service = ConfigService::new(loader).unwrap();
        assert!(
            service
                .stage_provider_credential("primary", "secret")
                .is_err()
        );
        let after = std::fs::read_to_string(&path).unwrap();
        assert_eq!(after, before);
        assert!(!after.contains("secret"));
    }

    #[test]
    fn chatgpt_plan_credentials_are_staged_in_the_dedicated_owner_slot() {
        let (service, _dir) = service();
        let abandoned_reference = service
            .stage_chatgpt_plan_credential("abandoned", "abandoned-token-bundle")
            .unwrap();
        let reference = service
            .stage_chatgpt_plan_credential("chatgpt", "protected-token-bundle")
            .unwrap();
        service
            .discard_staged_chatgpt_plan_credential(&abandoned_reference)
            .unwrap();
        assert!(
            service
                .read_chatgpt_plan_credential(&abandoned_reference)
                .is_err()
        );
        assert!(
            service
                .read_chatgpt_plan_credential(&reference)
                .unwrap()
                .contains("protected-token-bundle")
        );

        let ordinary_reference = service
            .stage_provider_credential("ordinary", "api-key")
            .unwrap();
        assert!(
            service
                .read_chatgpt_plan_credential(&ordinary_reference)
                .is_err()
        );

        service
            .edit(|config| {
                config.llm.providers.push(crate::config::ProviderConfig {
                    name: "chatgpt".into(),
                    provider: super::super::CHATGPT_PLAN_PROVIDER_IDENTITY.into(),
                    api_style: Some("openai-responses".into()),
                    base_url: super::super::CHATGPT_PLAN_BASE_URL.into(),
                    api_key_ref: Some(reference.clone()),
                    ..Default::default()
                });
                Ok(())
            })
            .unwrap();
        let active = service
            .chatgpt_plan_credential_reference("chatgpt")
            .unwrap();
        assert_eq!(active.as_deref(), Some(reference.as_str()));
        let staged_replacement = service
            .stage_chatgpt_plan_credential("chatgpt", "replacement-token-bundle")
            .unwrap();
        assert_eq!(
            service
                .chatgpt_plan_credential_reference("chatgpt")
                .unwrap()
                .as_deref(),
            Some(staged_replacement.as_str())
        );
        service
            .discard_staged_chatgpt_plan_credential(&staged_replacement)
            .unwrap();
        service
            .discard_staged_chatgpt_plan_credential(&reference)
            .unwrap();
        assert_eq!(
            service.read_chatgpt_plan_credential(&reference).unwrap(),
            "protected-token-bundle"
        );
    }

    #[test]
    fn staged_chatgpt_plan_credential_follows_provider_rename_before_settings_save() {
        let (service, _dir) = service();
        let reference = service
            .stage_chatgpt_plan_credential("original-name", "protected-token-bundle")
            .unwrap();
        let other_reference = service
            .stage_chatgpt_plan_credential("renamed", "other-token-bundle")
            .unwrap();
        assert!(
            service
                .reassign_staged_chatgpt_plan_credential(&reference, "renamed")
                .is_err()
        );
        service
            .discard_staged_chatgpt_plan_credential(&other_reference)
            .unwrap();
        service
            .reassign_staged_chatgpt_plan_credential(&reference, "renamed")
            .unwrap();
        assert_eq!(
            service
                .chatgpt_plan_credential_reference("renamed")
                .unwrap()
                .as_deref(),
            Some(reference.as_str())
        );

        service
            .edit(|config| {
                config.llm.providers.push(crate::config::ProviderConfig {
                    name: "renamed".into(),
                    provider: super::super::CHATGPT_PLAN_PROVIDER_IDENTITY.into(),
                    api_style: Some("openai-responses".into()),
                    base_url: super::super::CHATGPT_PLAN_BASE_URL.into(),
                    api_key_ref: Some(reference.clone()),
                    ..Default::default()
                });
                Ok(())
            })
            .unwrap();

        assert_eq!(
            service
                .chatgpt_plan_credential_reference("renamed")
                .unwrap()
                .as_deref(),
            Some(reference.as_str())
        );
        assert!(
            service
                .chatgpt_plan_credential_reference("original-name")
                .unwrap()
                .is_none()
        );
        assert_eq!(
            service.read_chatgpt_plan_credential(&reference).unwrap(),
            "protected-token-bundle"
        );
    }

    #[test]
    fn staged_chatgpt_plan_credential_can_replace_an_api_provider_draft() {
        let (service, _dir) = service();
        let api_reference = service
            .stage_provider_credential("convert-me", "api-key")
            .unwrap();
        service
            .edit(|config| {
                config.llm.providers.push(crate::config::ProviderConfig {
                    name: "convert-me".into(),
                    provider: "openai".into(),
                    api_style: Some("openai-responses".into()),
                    base_url: super::super::CHATGPT_PLAN_BASE_URL.into(),
                    api_key_ref: Some(api_reference.clone()),
                    ..Default::default()
                });
                Ok(())
            })
            .unwrap();

        let plan_reference = service
            .stage_chatgpt_plan_credential("convert-me", "protected-token-bundle")
            .unwrap();
        assert_eq!(
            service
                .chatgpt_plan_credential_reference("convert-me")
                .unwrap()
                .as_deref(),
            Some(plan_reference.as_str())
        );

        service
            .edit(|config| {
                let provider = config
                    .llm
                    .providers
                    .iter_mut()
                    .find(|provider| provider.name == "convert-me")
                    .unwrap();
                provider.provider = super::super::CHATGPT_PLAN_PROVIDER_IDENTITY.into();
                provider.api_style = Some("openai-responses".into());
                provider.base_url = super::super::CHATGPT_PLAN_BASE_URL.into();
                provider.api_key_ref = Some(plan_reference.clone());
                provider.api_key.clear();
                Ok(())
            })
            .unwrap();

        assert_eq!(
            service
                .read_chatgpt_plan_credential(&plan_reference)
                .unwrap(),
            "protected-token-bundle"
        );
    }

    #[test]
    fn startup_hydrates_current_references_without_rewriting_config() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let store = std::sync::Arc::new(InMemoryCredentialStore::default());
        let provider_ref = crate::types::new_id("cred");
        let ocr_ref = crate::types::new_id("cred");
        let mcp_ref = crate::types::new_id("cred");
        store.write(&provider_ref, "provider-secret").unwrap();
        store.write(&ocr_ref, "ocr-secret").unwrap();
        store.write(&mcp_ref, "mcp-secret").unwrap();

        let mut config = AppConfig::default();
        config.llm.providers.push(crate::config::ProviderConfig {
            name: "primary".into(),
            api_key_ref: Some(provider_ref),
            ..Default::default()
        });
        config.media.ocr.api_key_ref = Some(ocr_ref);
        config.mcp_servers.push(McpServerConfig {
            name: "example".into(),
            env_refs: vec![crate::config::McpEnvironmentCredentialRef {
                name: "TOKEN".into(),
                credential_ref: Some(mcp_ref),
                has_value: true,
            }],
            ..Default::default()
        });
        let original = toml::to_string_pretty(&config).unwrap();
        std::fs::write(&path, &original).unwrap();

        let loader = ConfigLoader::load_from(&path).unwrap();
        let service = ConfigService::new_with_credential_store(loader, store).unwrap();

        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
        let snapshot = service.snapshot().unwrap();
        assert_eq!(snapshot.config.llm.providers[0].api_key, "provider-secret");
        assert_eq!(snapshot.config.media.ocr.api_key, "ocr-secret");
        assert_eq!(snapshot.config.mcp_servers[0].env, ["TOKEN=mcp-secret"]);
        let settings_wire = serde_json::to_string(&service.settings().unwrap()).unwrap();
        for marker in ["provider-secret", "ocr-secret", "mcp-secret"] {
            assert!(!settings_wire.contains(marker));
        }
    }

    #[test]
    fn settings_patch_advances_version_and_notifies_changed_domains() {
        let (service, _dir) = service();
        let receiver = service.subscribe().unwrap();
        let mut settings = service.settings().unwrap();
        settings.session.max_steps_per_run += 1;
        settings.hotkey.key_binding = "Ctrl+Alt+H".into();

        let update = service
            .apply_patch(ConfigPatch::Settings(Box::new(SettingsPatch::from(
                &settings,
            ))))
            .unwrap();
        let change = update.change.clone().unwrap();

        assert_eq!(update.snapshot.version, 1);
        assert_eq!(change.version, 1);
        assert_eq!(
            change.domains,
            vec![ConfigDomain::Hotkey, ConfigDomain::Session]
        );
        assert_eq!(receiver.recv().unwrap(), change);
    }

    #[test]
    fn lifecycle_hook_patch_is_hot_versioned_and_not_exposed_in_settings() {
        let (service, dir) = service();
        let receiver = service.subscribe().unwrap();
        let hooks = super::super::LifecycleHooksConfig {
            run_start: vec![super::super::HookCommand {
                command: "hook.exe".into(),
                args: vec!["--run-start".into()],
                matcher: None,
            }],
            ..Default::default()
        };

        let update = service
            .apply_patch(ConfigPatch::LifecycleHooks(hooks.clone()))
            .unwrap();

        assert_eq!(update.snapshot.version, 1);
        assert_eq!(
            update.change.as_ref().unwrap().domains,
            vec![ConfigDomain::Hooks]
        );
        assert_eq!(receiver.recv().unwrap().domains, vec![ConfigDomain::Hooks]);
        assert_eq!(service.snapshot().unwrap().config.hooks, hooks);
        assert!(
            serde_json::to_string(&service.settings().unwrap())
                .unwrap()
                .contains("tool_settings")
        );
        assert!(
            !serde_json::to_string(&service.settings().unwrap())
                .unwrap()
                .contains("hook.exe")
        );

        let persisted = std::fs::read_to_string(dir.path().join("config.toml")).unwrap();
        assert!(persisted.contains("hook.exe"));
    }

    #[test]
    fn hook_workspace_trust_patches_are_atomic_and_exact() {
        let (service, _dir) = service();
        let workspace = tempfile::tempdir().unwrap();
        let path = crate::path::normalize_absolute_local_path(
            &std::fs::canonicalize(workspace.path()).unwrap(),
        )
        .unwrap();

        let trusted = service
            .apply_patch(ConfigPatch::TrustHookWorkspace(path.clone()))
            .unwrap();
        assert_eq!(
            trusted.snapshot.config.trusted_hook_workspaces,
            vec![path.clone()]
        );
        assert_eq!(trusted.change.unwrap().domains, vec![ConfigDomain::Hooks]);

        let duplicate = service
            .apply_patch(ConfigPatch::TrustHookWorkspace(path.clone()))
            .unwrap();
        assert!(duplicate.change.is_none());
        assert_eq!(duplicate.snapshot.version, 1);

        let revoked = service
            .apply_patch(ConfigPatch::UntrustHookWorkspace(path.clone()))
            .unwrap();
        assert!(revoked.snapshot.config.trusted_hook_workspaces.is_empty());
        assert_eq!(revoked.change.unwrap().domains, vec![ConfigDomain::Hooks]);
        assert!(
            service
                .apply_patch(ConfigPatch::UntrustHookWorkspace(path))
                .is_err()
        );
    }

    #[test]
    fn oversized_settings_patch_preserves_snapshot_version_and_file() {
        let (service, dir) = service();
        let path = dir.path().join("config.toml");
        let original = std::fs::read(&path).unwrap();
        let before = service.snapshot().unwrap();
        let mut settings = service.settings().unwrap();
        settings.llm.providers.push(crate::config::ProviderConfig {
            name: "primary".into(),
            no_proxy: Some("x".repeat(4 * 1024 * 1024 + 1)),
            ..Default::default()
        });

        let error = service
            .apply_patch(ConfigPatch::Settings(Box::new(SettingsPatch::from(
                &settings,
            ))))
            .unwrap_err();

        assert!(error.to_string().contains("serialized config exceeds"));
        assert_eq!(service.snapshot().unwrap(), before);
        assert_eq!(std::fs::read(path).unwrap(), original);
    }

    #[test]
    fn no_op_patch_does_not_write_or_notify() {
        let (service, _dir) = service();
        let receiver = service.subscribe().unwrap();
        let settings = service.settings().unwrap();

        let update = service
            .apply_patch(ConfigPatch::Settings(Box::new(SettingsPatch::from(
                &settings,
            ))))
            .unwrap();

        assert!(update.change.is_none());
        assert_eq!(update.snapshot.version, 0);
        assert!(receiver.try_recv().is_err());
    }

    #[test]
    fn slow_subscriber_is_disconnected_when_bounded_queue_fills() {
        let (service, _dir) = service();
        let receiver = service.subscribe().unwrap();
        let mut changes = Vec::with_capacity(CONFIG_CHANGE_NOTIFICATION_CAPACITY + 1);

        for _ in 0..=CONFIG_CHANGE_NOTIFICATION_CAPACITY {
            let mut settings = service.settings().unwrap();
            settings.session.max_steps_per_run += 1;
            let update = service
                .apply_patch(ConfigPatch::Settings(Box::new(SettingsPatch::from(
                    &settings,
                ))))
                .unwrap();
            changes.push(update.change.unwrap());
        }

        assert_eq!(
            service.snapshot().unwrap().version,
            (CONFIG_CHANGE_NOTIFICATION_CAPACITY + 1) as ConfigVersion
        );
        for expected in changes.iter().take(CONFIG_CHANGE_NOTIFICATION_CAPACITY) {
            assert_eq!(receiver.recv().unwrap(), *expected);
        }
        assert!(matches!(
            receiver.try_recv(),
            Err(mpsc::TryRecvError::Disconnected)
        ));

        let fresh_receiver = service.subscribe().unwrap();
        let mut settings = service.settings().unwrap();
        settings.session.max_steps_per_run += 1;
        let update = service
            .apply_patch(ConfigPatch::Settings(Box::new(SettingsPatch::from(
                &settings,
            ))))
            .unwrap();
        assert_eq!(fresh_receiver.recv().unwrap(), update.change.unwrap());
    }

    #[test]
    fn invalid_mcp_server_names_are_rejected_without_mutating_config() {
        let (service, _dir) = service();
        let before = service.snapshot().unwrap();
        let invalid = vec![McpServerConfig {
            name: "server.name".into(),
            ..Default::default()
        }];

        assert!(
            service
                .apply_patch(ConfigPatch::McpServers(invalid))
                .is_err()
        );
        assert_eq!(service.snapshot().unwrap(), before);
    }

    #[test]
    fn unsafe_numeric_settings_are_rejected_without_mutating_config() {
        let (service, _dir) = service();
        let before = service.snapshot().unwrap();
        let mut settings = SettingsSnapshot::from(&before.config);
        settings.context_limits.max_upload_total_bytes = 9_007_199_254_740_992;

        assert!(
            service
                .apply_patch(ConfigPatch::Settings(Box::new(SettingsPatch::from(
                    &settings
                ))))
                .is_err()
        );
        assert_eq!(service.snapshot().unwrap(), before);
    }

    #[test]
    fn failed_edit_does_not_change_snapshot_or_version() {
        let (service, _dir) = service();
        let before = service.snapshot().unwrap();
        let result = service.edit::<()>(|config| -> anyhow::Result<()> {
            config.session.max_steps_per_run = 1;
            anyhow::bail!("reject test mutation")
        });

        assert!(result.is_err());
        assert_eq!(service.snapshot().unwrap(), before);
    }

    #[test]
    fn external_config_edit_is_not_overwritten_by_a_stale_service_snapshot() {
        let (service, _dir) = service();
        let before = service.snapshot().unwrap();
        let path = service.path().unwrap();
        let mut external = before.config.clone();
        external.session.max_concurrent = 9;
        let external_toml = toml::to_string_pretty(&external).unwrap();
        std::fs::write(&path, &external_toml).unwrap();

        let result = service.edit(|config| {
            config.session.max_concurrent = 4;
            Ok(())
        });

        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("changed outside Haven")
        );
        assert_eq!(service.snapshot().unwrap(), before);
        assert_eq!(std::fs::read_to_string(path).unwrap(), external_toml);
    }

    #[test]
    fn model_patch_is_typed_and_reports_llm_domain() {
        let (service, _dir) = service();
        let mut config = service.snapshot().unwrap().config;
        config.llm.set_model_config(
            "default_model",
            ModelConfig {
                provider_name: "openai".into(),
                model: "old-model".into(),
                ..Default::default()
            },
        );
        service
            .edit(|current| {
                *current = config;
                Ok(())
            })
            .unwrap();

        let update = service
            .apply_patch(ConfigPatch::LlmModel {
                model_config_id: "default_model".into(),
                patch: LlmModelPatch::Model("new-model".into()),
            })
            .unwrap();
        assert_eq!(
            update
                .snapshot
                .config
                .llm
                .model_config("default_model")
                .unwrap()
                .model,
            "new-model"
        );
        assert_eq!(update.change.unwrap().domains, vec![ConfigDomain::Llm]);
    }
}
