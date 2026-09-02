//! Versioned configuration snapshots and serialized persistence.
//!
//! `ConfigLoader` remains the TOML codec and file-format boundary. This module
//! owns the live configuration snapshot, mutation serialization, versioning,
//! typed patches, and change notifications so callers do not coordinate a
//! shared loader mutex themselves.

use super::{
    AppConfig, ConfigLoader, EndpointRole, LlmConfig, LogLevel, McpDiscoveryConfig,
    McpServerConfig, RoleConfig, SecurityConfig, Settings, SkillsConfig, SkillsExecConfig,
    ToolConfig,
};
use crate::types::ShellChoice;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Mutex, PoisonError};

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
/// `ReplaceAppConfig` is a deliberately narrow migration escape hatch for
/// domain adapters that already validate a complete `AppConfig`; it should be
/// removed once all specialized admin operations use typed variants.
#[derive(Debug, Clone)]
pub enum ConfigPatch {
    Settings(Settings),
    ReplaceAppConfig(AppConfig),
    Llm(LlmConfig),
    LlmRole {
        role: EndpointRole,
        patch: LlmRolePatch,
    },
    DefaultShell(ShellChoice),
    Security(SecurityConfig),
    SecurityPermissions(Vec<super::StoredPermission>),
    Media(super::MediaConfig),
    Skills {
        config: SkillsConfig,
        exec: SkillsExecConfig,
    },
    McpDiscovery(McpDiscoveryConfig),
    McpServers(Vec<McpServerConfig>),
    Tools(HashMap<String, ToolConfig>),
    LogLevel(LogLevel),
}

/// Field-level patch for a configured model role.
#[derive(Debug, Clone)]
pub enum LlmRolePatch {
    Replace(RoleConfig),
    Model(String),
    ReasoningEffort(Option<String>),
    WebSearch(Option<String>),
}

impl ConfigPatch {
    fn apply(self, config: &mut AppConfig) -> anyhow::Result<()> {
        match self {
            Self::Settings(settings) => config.apply_settings(&settings),
            Self::ReplaceAppConfig(updated) => *config = updated,
            Self::Llm(llm) => config.llm = llm,
            Self::LlmRole { role, patch } => {
                let slot = config.llm.role_mut(role).ok_or_else(|| {
                    anyhow::anyhow!("unknown or unconfigured role: {}", role.as_str())
                })?;
                match patch {
                    LlmRolePatch::Replace(mut updated) => {
                        updated.stamp_role(role.as_str());
                        *slot = updated;
                    }
                    LlmRolePatch::Model(model) => slot.model = model,
                    LlmRolePatch::ReasoningEffort(effort) => slot.reasoning_effort = effort,
                    LlmRolePatch::WebSearch(mode) => slot.web_search = mode,
                }
            }
            Self::DefaultShell(shell) => config.default_shell = shell,
            Self::Security(security) => config.security = security,
            Self::SecurityPermissions(permissions) => config.security.permissions = permissions,
            Self::Media(media) => config.media = media,
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
            Self::LogLevel(level) => config.log.level = level,
        }
        Ok(())
    }
}

struct ConfigState {
    loader: ConfigLoader,
    version: ConfigVersion,
}

/// The single live configuration owner for an application process.
pub struct ConfigService {
    state: Mutex<ConfigState>,
    subscribers: Mutex<Vec<Sender<ConfigChanged>>>,
}

impl std::fmt::Debug for ConfigService {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ConfigService")
            .finish_non_exhaustive()
    }
}

impl ConfigService {
    pub fn new(loader: ConfigLoader) -> Self {
        Self {
            state: Mutex::new(ConfigState { loader, version: 0 }),
            subscribers: Mutex::new(Vec::new()),
        }
    }

    pub fn load() -> anyhow::Result<Self> {
        Ok(Self::new(ConfigLoader::load()?))
    }

    pub fn snapshot(&self) -> anyhow::Result<ConfigSnapshot> {
        let state = self.lock_state()?;
        Ok(ConfigSnapshot {
            version: state.version,
            config: state.loader.config().clone(),
        })
    }

    /// Return a detached loader view for read-only adapters that have not yet
    /// migrated to `ConfigSnapshot`.
    pub fn loader(&self) -> anyhow::Result<ConfigLoader> {
        Ok(self.lock_state()?.loader.clone())
    }

    pub fn settings(&self) -> anyhow::Result<Settings> {
        let snapshot = self.snapshot()?;
        Ok(Settings::from(&snapshot.config))
    }

    pub fn path(&self) -> anyhow::Result<std::path::PathBuf> {
        Ok(self.lock_state()?.loader.path().to_path_buf())
    }

    /// Subscribe to successful durable mutations. The receiver is detached
    /// from the mutation lock; a slow or dropped subscriber cannot block a
    /// config write.
    pub fn subscribe(&self) -> anyhow::Result<Receiver<ConfigChanged>> {
        let (sender, receiver) = mpsc::channel();
        self.subscribers.lock().map_err(lock_error)?.push(sender);
        Ok(receiver)
    }

    /// Apply one typed patch, persist it atomically, advance the version and
    /// publish a secret-free change notification.
    pub fn apply_patch(&self, patch: ConfigPatch) -> anyhow::Result<ConfigUpdate<()>> {
        self.edit(|config| patch.apply(config))
    }

    /// Transitional adapter for domain code that already owns a validated
    /// typed operation. The closure runs while the serialized config state is
    /// locked; a failed save restores the previous in-memory snapshot.
    pub fn edit<T>(
        &self,
        edit: impl FnOnce(&mut AppConfig) -> anyhow::Result<T>,
    ) -> anyhow::Result<ConfigUpdate<T>> {
        self.edit_loader(|loader| edit(loader.config_mut()))
    }

    /// Transitional adapter for existing domain code that needs the loader
    /// shape. It still uses the service's single lock, atomic save, version,
    /// and notification path; callers must not call `ConfigLoader::save`.
    pub fn edit_loader<T>(
        &self,
        edit: impl FnOnce(&mut ConfigLoader) -> anyhow::Result<T>,
    ) -> anyhow::Result<ConfigUpdate<T>> {
        let (update, change) = {
            let mut state = self.lock_state_mut()?;
            let before = state.loader.config().clone();
            let value = match edit(&mut state.loader) {
                Ok(value) => value,
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
                if let Err(error) = state.loader.save() {
                    *state.loader.config_mut() = before;
                    return Err(error);
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
        subscribers.retain(|subscriber| subscriber.send(change.clone()).is_ok());
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
    domains
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn service() -> (ConfigService, tempfile::TempDir) {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let loader = ConfigLoader::load_from(&path).unwrap();
        (ConfigService::new(loader), dir)
    }

    #[test]
    fn settings_patch_advances_version_and_notifies_changed_domains() {
        let (service, _dir) = service();
        let receiver = service.subscribe().unwrap();
        let mut settings = service.settings().unwrap();
        settings.session.max_steps += 1;
        settings.hotkey.key_binding = "Ctrl+Alt+H".into();

        let update = service
            .apply_patch(ConfigPatch::Settings(settings))
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
    fn no_op_patch_does_not_write_or_notify() {
        let (service, _dir) = service();
        let receiver = service.subscribe().unwrap();
        let settings = service.settings().unwrap();

        let update = service
            .apply_patch(ConfigPatch::Settings(settings))
            .unwrap();

        assert!(update.change.is_none());
        assert_eq!(update.snapshot.version, 0);
        assert!(receiver.try_recv().is_err());
    }

    #[test]
    fn failed_edit_does_not_change_snapshot_or_version() {
        let (service, _dir) = service();
        let before = service.snapshot().unwrap();
        let result = service.edit::<()>(|config| -> anyhow::Result<()> {
            config.session.max_steps = 1;
            anyhow::bail!("reject test mutation")
        });

        assert!(result.is_err());
        assert_eq!(service.snapshot().unwrap(), before);
    }

    #[test]
    fn role_patch_is_typed_and_reports_llm_domain() {
        let (service, _dir) = service();
        let mut config = service.snapshot().unwrap().config;
        config.llm.set_role(
            EndpointRole::DefaultModel,
            RoleConfig {
                provider: "openai".into(),
                model: "old-model".into(),
                ..Default::default()
            },
        );
        service
            .apply_patch(ConfigPatch::ReplaceAppConfig(config))
            .unwrap();

        let update = service
            .apply_patch(ConfigPatch::LlmRole {
                role: EndpointRole::DefaultModel,
                patch: LlmRolePatch::Model("new-model".into()),
            })
            .unwrap();
        assert_eq!(
            update
                .snapshot
                .config
                .llm
                .role(EndpointRole::DefaultModel)
                .unwrap()
                .model,
            "new-model"
        );
        assert_eq!(update.change.unwrap().domains, vec![ConfigDomain::Llm]);
    }
}
