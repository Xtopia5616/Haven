//! Domain services shared by the five typed administration operations.
//!
//! This module deliberately contains no operation selector. Each public method
//! accepts the fields for exactly one operation; the typed operation wrappers in
//! `admin.rs` own selection, schema, policy, and error conversion.

use super::{AdminContext, McpAddFields, McpRefreshPlan, McpUpdateFields};
use crate::ToolRegistry;
use anyhow::{Error, Result};
use haven_common::config::{
    AppConfig, ConfigLoader, ConfigPatch, ConfigVersion, LogConfig, LogLevel, McpServerConfig,
    RequestKind, RouterConfig, Settings, endpoint_credentials_ready,
};
use haven_common::types::McpTransportType;
use haven_mcp::McpClientStatus;
use serde::Serialize;
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};
use std::future::Future;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;

use haven_mcp::McpManager;
use haven_skills::{SkillInfo, SkillRegistry};

const DIAGNOSTIC_MODEL_HEALTH_CHECK_TIMEOUT: Duration = Duration::from_secs(7);

pub(crate) enum NativeMcpServiceError {
    Preflight(String),
    BeforeSideEffect(Error),
    SideEffect(Error),
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct LogsLevelResult {
    pub(crate) level: LogLevel,
    pub(crate) saved: bool,
    pub(crate) config_version: ConfigVersion,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ToolSetResult {
    pub(crate) name: String,
    pub(crate) enabled: bool,
    pub(crate) saved: bool,
    pub(crate) note: &'static str,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct DiagnosticsStatus {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) config_path: Option<String>,
    /// Masked configuration settings remain a dynamic config-tree payload.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) settings: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) config_error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) models: Option<BTreeMap<String, DiagnosticModelStatus>>,
    pub(crate) tools: DiagnosticToolsStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) mcp: Option<Vec<McpStatusOutput>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) mcp_error: Option<String>,
    pub(crate) skills: Vec<SkillCatalogStatusOutput>,
    pub(crate) sessions: DiagnosticSessionsOutput,
    pub(crate) log: DiagnosticLogOutput,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct DiagnosticModelStatus {
    pub(crate) configured: bool,
    pub(crate) status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) route_issue: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) model_config_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) provider: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) provider_model_id: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct DiagnosticToolsStatus {
    pub(crate) count: usize,
    pub(crate) names: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct SkillCatalogStatusOutput {
    pub(crate) name: String,
    pub(crate) enabled: bool,
    pub(crate) executable: bool,
    pub(crate) description: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) unavailable_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) manifest_error: Option<String>,
}

impl From<SkillInfo> for SkillCatalogStatusOutput {
    fn from(skill: SkillInfo) -> Self {
        Self {
            name: skill.name,
            enabled: skill.enabled,
            executable: skill.executable,
            description: skill.description,
            unavailable_reason: skill.unavailable_reason,
            manifest_error: skill.manifest_error,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
pub(crate) enum DiagnosticSessionsOutput {
    Available {
        total: i64,
        recent_50_by_status: BTreeMap<String, usize>,
    },
    Unavailable {
        unavailable: bool,
    },
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct DiagnosticLogOutput {
    pub(crate) path: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
pub(crate) enum LogsTailOutput {
    Read {
        path: String,
        total_lines: usize,
        lines: Vec<String>,
    },
    Unavailable {
        path: String,
        error: String,
    },
}

#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
pub(crate) enum SessionsOutput {
    Available { sessions: Vec<SessionSummaryOutput> },
    Unavailable { unavailable: bool },
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct SessionSummaryOutput {
    pub(crate) id: String,
    pub(crate) status: haven_common::SessionStatus,
    pub(crate) title: Option<String>,
    pub(crate) input_chars: usize,
    pub(crate) created_at: String,
    pub(crate) updated_at: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
pub(crate) enum ErrorsOutput {
    Available { errors: Vec<SessionErrorOutput> },
    Unavailable { unavailable: bool },
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct SessionErrorOutput {
    pub(crate) id: String,
    pub(crate) title: Option<String>,
    pub(crate) input_chars: usize,
    pub(crate) created_at: String,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct SkillsListOutput {
    pub(crate) skills: Vec<SkillSummaryOutput>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct SkillSummaryOutput {
    #[serde(flatten)]
    pub(crate) status: SkillCatalogStatusOutput,
    pub(crate) root: String,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct SkillSetOutput {
    pub(crate) name: String,
    pub(crate) enabled: bool,
    pub(crate) saved: bool,
    pub(crate) note: &'static str,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct SkillCreateOutput {
    pub(crate) name: String,
    pub(crate) created: bool,
    pub(crate) root: String,
    pub(crate) has_script: bool,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct McpStatusOutput {
    pub(crate) name: String,
    pub(crate) enabled: bool,
    pub(crate) connected: bool,
    pub(crate) tools: Option<usize>,
    pub(crate) last_error: String,
    /// This field intentionally serializes as `null` when no diagnostic exists.
    pub(crate) diagnostic: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct McpConnectionOutput {
    pub(crate) name: String,
    pub(crate) connected: bool,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct McpAddOutput {
    pub(crate) name: String,
    pub(crate) enabled: bool,
    pub(crate) saved: bool,
    pub(crate) connected: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) warning: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct McpConfigUpdateOutput {
    pub(crate) name: String,
    pub(crate) enabled: bool,
    pub(crate) saved: bool,
    pub(crate) connected: bool,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct McpRemoveOutput {
    pub(crate) name: String,
    pub(crate) removed: bool,
    pub(crate) connected: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
pub(crate) enum McpReloadConnectionOutput {
    Connected {
        name: String,
        connected: bool,
    },
    Failed {
        name: String,
        connected: bool,
        error: String,
    },
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct McpReloadOutput {
    pub(crate) reloaded: bool,
    pub(crate) connected: Vec<McpReloadConnectionOutput>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct McpRefreshOutput {
    pub(crate) added: Vec<String>,
    pub(crate) removed: Vec<String>,
    pub(crate) updated: Vec<String>,
    pub(crate) failed: Vec<String>,
}

/// Runtime implementation dependencies for the five capability-scoped admin
/// surfaces. The context is app-level; the skills/MCP/catalog dependencies are
/// supplied by the builtin composition root.
pub(crate) struct AdminServices {
    pub(crate) context: AdminContext,
    pub(crate) skill_registry: SkillRegistry,
    pub(crate) mcp_manager: Arc<McpManager>,
    pub(crate) server_configs: Arc<RwLock<HashMap<String, McpServerConfig>>>,
    pub(crate) registry: ToolRegistry,
    pub(crate) max_instructions_bytes: usize,
    pub(crate) max_script_bytes: usize,
}

impl AdminServices {
    pub(crate) fn new(
        context: AdminContext,
        skill_registry: SkillRegistry,
        mcp_manager: Arc<McpManager>,
        server_configs: Arc<RwLock<HashMap<String, McpServerConfig>>>,
        registry: ToolRegistry,
        max_instructions_bytes: usize,
        max_script_bytes: usize,
    ) -> Self {
        Self {
            context,
            skill_registry,
            mcp_manager,
            server_configs,
            registry,
            max_instructions_bytes,
            max_script_bytes,
        }
    }

    pub(crate) fn read_config(&self) -> Result<AppConfig> {
        match &self.context.config_service {
            Some(service) => Ok(service.snapshot()?.config),
            None => Ok(ConfigLoader::load()?.config().clone()),
        }
    }

    pub(crate) fn config_path(&self) -> Result<PathBuf> {
        match &self.context.config_service {
            Some(service) => Ok(service.path()?),
            None => Ok(ConfigLoader::default_path()),
        }
    }

    fn config_service(&self) -> Result<&haven_common::config::ConfigService> {
        self.context
            .config_service
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("configuration administration is unavailable"))
    }

    async fn lock_config_apply(&self) -> Option<tokio::sync::OwnedMutexGuard<()>> {
        match &self.context.config_apply_gate {
            Some(gate) => Some(Arc::clone(gate).lock_owned().await),
            None => None,
        }
    }

    async fn rebuild_catalog(&self) -> Result<()> {
        if let Some(tool_control) = &self.context.tool_control {
            tool_control.rebuild_catalog().await?;
        }
        Ok(())
    }

    pub(crate) async fn config_get(&self, path: Option<&str>) -> Result<Value> {
        let config = self.read_config()?;
        let Some(path) = path.filter(|path| !path.is_empty()) else {
            let mut settings = serde_json::to_value(Settings::from(&config))?;
            super::super::admin_support::mask_sensitive_config(&mut settings);
            return Ok(settings);
        };
        let mut root = serde_json::to_value(&config)?;
        super::super::admin_support::mask_sensitive_config(&mut root);
        super::super::admin_support::value_at(&root, path)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("config key '{}' not found", path))
    }

    pub(crate) async fn logs_level(&self, level: LogLevel) -> Result<LogsLevelResult> {
        let _config_apply_guard = self.lock_config_apply().await;
        let update = self
            .config_service()?
            .apply_patch(ConfigPatch::LogLevel(level.clone()))?;
        if let Some(log_level) = &self.context.log_level {
            log_level.set_level(&level)?;
        }
        Ok(LogsLevelResult {
            level,
            saved: true,
            config_version: update.snapshot.version,
        })
    }

    pub(crate) async fn tool_set(&self, name: &str, enabled: bool) -> Result<ToolSetResult> {
        let _config_apply_guard = self.lock_config_apply().await;
        let mut settings = self.config_service()?.snapshot()?.config.tool_settings;
        settings.entry(name.to_string()).or_default().enabled = enabled;
        self.config_service()?
            .apply_patch(ConfigPatch::Tools(settings))?;
        if let Some(tool_control) = &self.context.tool_control {
            tool_control.set_tool_enabled(name, enabled).await?;
        }
        Ok(ToolSetResult {
            name: name.to_string(),
            enabled,
            saved: true,
            note: "take effect immediately",
        })
    }

    pub(crate) async fn diagnostics_status(&self) -> Result<DiagnosticsStatus> {
        let mut config_path = None;
        let mut settings = None;
        let mut config_error = None;
        match self.read_config() {
            Ok(config) => {
                config_path = Some(self.config_path()?.to_string_lossy().to_string());
                let mut masked_settings = serde_json::to_value(Settings::from(&config))?;
                super::super::admin_support::mask_sensitive_config(&mut masked_settings);
                settings = Some(masked_settings);
            }
            Err(error) => {
                config_error = Some(sanitize_diagnostic(&error.to_string()));
            }
        }

        let models = if let Some(router) = &self.context.router {
            let router_config = router.config().await.clone();
            let route_details = RequestKind::ALL
                .iter()
                .copied()
                .map(|request| (request, diagnostic_route_metadata(&router_config, request)))
                .collect::<Vec<_>>();
            let configured = route_details
                .iter()
                .map(|(request, metadata)| (*request, metadata.configured))
                .collect::<Vec<_>>();
            let health_router = Arc::clone(router);
            let mut models = collect_diagnostic_model_health(
                configured,
                DIAGNOSTIC_MODEL_HEALTH_CHECK_TIMEOUT,
                move |request| {
                    let router = Arc::clone(&health_router);
                    async move {
                        router
                            .health_check(haven_llm::types::HealthCheckRequest { request })
                            .await
                            .map_err(|error| sanitize_diagnostic(&error.to_string()))
                    }
                },
            )
            .await;
            for (request, metadata) in route_details {
                if let Some(status) = models.get_mut(request.as_str()) {
                    status.route_issue = metadata.route_issue.map(str::to_string);
                    status.model_config_id = metadata.model_config_id;
                    status.provider = metadata.provider;
                    status.provider_model_id = metadata.provider_model_id;
                }
            }
            Some(models)
        } else {
            None
        };

        let schemas = self.registry.list_schemas().await;
        let names: Vec<String> = schemas
            .iter()
            .filter_map(|schema| schema["name"].as_str().map(str::to_string))
            .collect();
        let tools = DiagnosticToolsStatus {
            count: schemas.len(),
            names,
        };

        let (mcp, mcp_error) = match self.mcp_status().await {
            Ok(mcp) => (Some(mcp), None),
            Err(error) => (None, Some(sanitize_diagnostic(&error.to_string()))),
        };

        let skills: Vec<SkillCatalogStatusOutput> = self
            .skill_registry
            .list_skill_infos()
            .await
            .into_iter()
            .map(SkillCatalogStatusOutput::from)
            .collect();

        let sessions = if let Some(session_store) = &self.context.session_store {
            let mut counts: BTreeMap<String, usize> = BTreeMap::new();
            match session_store.list_session_history(50, 0).await {
                Ok(sessions) => {
                    for session in &sessions {
                        *counts
                            .entry(session.status.as_str().to_string())
                            .or_default() += 1;
                    }
                }
                Err(error) => {
                    tracing::warn!(error = %error, "admin diagnostics list_session_history failed")
                }
            }
            let total = match session_store.count_session_history().await {
                Ok(total) => total,
                Err(error) => {
                    tracing::warn!(error = %error, "admin diagnostics count_sessions failed");
                    0
                }
            };
            DiagnosticSessionsOutput::Available {
                total,
                recent_50_by_status: counts,
            }
        } else {
            DiagnosticSessionsOutput::Unavailable { unavailable: true }
        };

        let configured_log_path = self
            .context
            .log_path
            .clone()
            .unwrap_or_else(LogConfig::default_log_path);
        let log_path = if self.context.file_logging_enabled {
            haven_common::log_file::resolve_current_log_file(&configured_log_path)
                .ok()
                .flatten()
                .unwrap_or(configured_log_path)
        } else {
            configured_log_path
        }
        .to_string_lossy()
        .to_string();
        Ok(DiagnosticsStatus {
            config_path,
            settings,
            config_error,
            models,
            tools,
            mcp,
            mcp_error,
            skills,
            sessions,
            log: DiagnosticLogOutput { path: log_path },
        })
    }

    pub(crate) async fn logs_tail(&self, limit: Option<i64>) -> Result<LogsTailOutput> {
        let limit = limit.unwrap_or(50).clamp(1, 500) as usize;
        let file_logging_enabled = self.context.file_logging_enabled
            && match &self.context.config_service {
                Some(config_service) => config_service.snapshot()?.config.log.file_enabled,
                None => true,
            };
        let configured_path = self
            .context
            .log_path
            .clone()
            .unwrap_or_else(LogConfig::default_log_path);
        let configured_path_text = configured_path.to_string_lossy().to_string();
        if !file_logging_enabled {
            return Ok(LogsTailOutput::Unavailable {
                path: configured_path_text,
                error: "file logging is disabled".into(),
            });
        }

        let read_result = tokio::task::spawn_blocking(
            move || -> std::io::Result<Option<(PathBuf, usize, Vec<String>)>> {
                let Some(path) =
                    haven_common::log_file::resolve_current_log_file(&configured_path)?
                else {
                    return Ok(None);
                };
                let (lines, total_lines) =
                    haven_common::log_file::read_tail_lines_with_count(&path, limit)?;
                Ok(Some((path, total_lines, lines)))
            },
        )
        .await
        .map_err(Error::from)?;

        match read_result {
            Ok(Some((path, total_lines, lines))) => Ok(LogsTailOutput::Read {
                path: path.to_string_lossy().to_string(),
                total_lines,
                lines: lines.iter().map(|line| sanitize_log_line(line)).collect(),
            }),
            Ok(None) => Ok(LogsTailOutput::Unavailable {
                path: configured_path_text,
                error: "no log file found yet".into(),
            }),
            Err(error) => Ok(LogsTailOutput::Unavailable {
                path: configured_path_text,
                error: format!("cannot read log file: {error}"),
            }),
        }
    }

    pub(crate) async fn sessions(&self, limit: Option<i64>) -> Result<SessionsOutput> {
        let Some(session_store) = &self.context.session_store else {
            return Ok(SessionsOutput::Unavailable { unavailable: true });
        };
        let sessions = session_store
            .list_session_history(limit.unwrap_or(10).clamp(1, 50), 0)
            .await?;
        let rows: Vec<SessionSummaryOutput> = sessions
            .into_iter()
            .map(|session| SessionSummaryOutput {
                id: session.id,
                status: session.status,
                title: session.title,
                input_chars: session.input_text.chars().count(),
                created_at: session.created_at,
                updated_at: session.updated_at,
            })
            .collect();
        Ok(SessionsOutput::Available { sessions: rows })
    }

    pub(crate) async fn errors(&self, limit: Option<i64>) -> Result<ErrorsOutput> {
        let Some(session_store) = &self.context.session_store else {
            return Ok(ErrorsOutput::Unavailable { unavailable: true });
        };
        let sessions = session_store
            .list_session_history(limit.unwrap_or(10).clamp(1, 50), 0)
            .await?;
        let rows: Vec<SessionErrorOutput> = sessions
            .into_iter()
            .filter(|session| session.status == haven_common::SessionStatus::Error)
            .map(|session| SessionErrorOutput {
                id: session.id,
                title: session.title,
                input_chars: session.input_text.chars().count(),
                created_at: session.created_at,
            })
            .collect();
        Ok(ErrorsOutput::Available { errors: rows })
    }

    pub(crate) async fn skills_list(&self) -> Result<SkillsListOutput> {
        let skills: Vec<SkillSummaryOutput> = self
            .skill_registry
            .list_skill_infos()
            .await
            .into_iter()
            .map(|skill| {
                let root = skill.root.clone();
                SkillSummaryOutput {
                    status: skill.into(),
                    root,
                }
            })
            .collect();
        Ok(SkillsListOutput { skills })
    }

    pub(crate) async fn skill_set(&self, name: &str, enabled: bool) -> Result<SkillSetOutput> {
        let _config_apply_guard = self.lock_config_apply().await;
        let config_service = Arc::clone(
            self.context
                .config_service
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("configuration administration is unavailable"))?,
        );
        if self.skill_registry.get_skill(name).await.is_none() {
            anyhow::bail!("skill '{}' not found", name);
        }
        self.skill_registry.set_enabled(name, enabled).await?;
        let enabled_skill_allowlist = self.skill_registry.enabled_skill_allowlist().await;
        let snapshot = config_service.snapshot()?;
        let mut skills = snapshot.config.skills;
        skills.enabled = enabled_skill_allowlist;
        if let Err(error) = config_service.apply_patch(ConfigPatch::Skills {
            config: skills,
            exec: snapshot.config.skills_exec,
        }) {
            if let Err(rollback) = self.skill_registry.set_enabled(name, !enabled).await {
                tracing::error!(
                    skill = name,
                    error = %haven_common::error::sanitize_error_text(&rollback.to_string()),
                    "skill enable rollback failed after config persistence error"
                );
            }
            return Err(error);
        }
        self.rebuild_catalog().await?;
        Ok(SkillSetOutput {
            name: name.to_string(),
            enabled,
            saved: true,
            note: "take effect immediately for new loads",
        })
    }

    pub(crate) async fn skill_create(
        &self,
        name: &str,
        description: &str,
        instructions: &str,
        language: Option<&str>,
        version: Option<&str>,
        script: Option<&str>,
    ) -> Result<SkillCreateOutput> {
        let _config_apply_guard = self.lock_config_apply().await;
        let config_service = Arc::clone(
            self.context
                .config_service
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("configuration administration is unavailable"))?,
        );
        haven_skills::validate_skill_name(name)?;
        if description.is_empty() {
            anyhow::bail!("description is required");
        }
        if instructions.is_empty() {
            anyhow::bail!("instructions are required (the '## Instructions' body)");
        }
        if instructions.len() > self.max_instructions_bytes {
            anyhow::bail!(
                "instructions too large (max {} bytes)",
                self.max_instructions_bytes
            );
        }
        let language = language.unwrap_or("python");
        if language != "python" {
            anyhow::bail!("unsupported language '{language}': only 'python' is supported");
        }
        let version = version
            .map(|value| value.replace(['\n', '\r'], " ").trim().to_string())
            .filter(|value| !value.is_empty());
        if let Some(version) = &version
            && version.len() > 64
        {
            anyhow::bail!("version too long (max 64 characters)");
        }
        if let Some(script) = script
            && script.len() > self.max_script_bytes
        {
            anyhow::bail!("script too large (max {} bytes)", self.max_script_bytes);
        }
        let script = script.ok_or_else(|| {
            anyhow::anyhow!(
                "script is required: executable Haven skills need an entry script under scripts/"
            )
        })?;
        if script.trim().is_empty() {
            anyhow::bail!("script must not be empty");
        }

        let root = self.skill_registry.resolved_root().await;
        let skill_dir = root.join(name);
        if skill_dir.exists() {
            anyhow::bail!("skill '{}' already exists at {}", name, skill_dir.display());
        }
        tokio::fs::create_dir_all(&skill_dir).await?;
        let desc_line = description.replace(['\n', '\r'], " ");
        let mut markdown =
            format!("# Skill: {name}\n\n## Metadata\n- name: {name}\n- description: {desc_line}\n");
        if let Some(version) = &version {
            markdown.push_str(&format!("- version: {version}\n"));
        }
        markdown.push_str(&format!(
            "- language: {language}\n\n## Instructions\n{instructions}\n"
        ));
        tokio::fs::write(skill_dir.join("SKILL.md"), markdown).await?;

        let scripts = skill_dir.join("scripts");
        tokio::fs::create_dir_all(&scripts).await?;
        tokio::fs::write(scripts.join("main.py"), script).await?;
        let has_script = true;

        self.skill_registry.refresh_from_disk().await?;
        self.skill_registry.set_enabled(name, true).await?;
        let enabled_skill_allowlist = self.skill_registry.enabled_skill_allowlist().await;
        let snapshot = config_service.snapshot()?;
        let mut skills = snapshot.config.skills;
        skills.enabled = enabled_skill_allowlist;
        if let Err(error) = config_service.apply_patch(ConfigPatch::Skills {
            config: skills,
            exec: snapshot.config.skills_exec,
        }) {
            if let Err(rollback) = tokio::fs::remove_dir_all(&skill_dir).await {
                tracing::warn!(
                    error = %haven_common::error::sanitize_error_text(&rollback.to_string()),
                    "skill creation rollback failed"
                );
            }
            if let Err(refresh_error) = self.skill_registry.refresh_from_disk().await {
                tracing::warn!(error = %haven_common::error::sanitize_error_text(&refresh_error.to_string()), "skill catalog refresh failed while rolling back");
            }
            return Err(error);
        }
        self.rebuild_catalog().await?;
        Ok(SkillCreateOutput {
            name: name.to_string(),
            created: true,
            root: skill_dir.to_string_lossy().to_string(),
            has_script,
        })
    }

    pub(crate) async fn mcp_status(&self) -> Result<Vec<McpStatusOutput>> {
        let servers: Vec<McpServerConfig> = {
            let configs = self.server_configs.read().await;
            if !configs.is_empty() {
                configs.values().cloned().collect()
            } else {
                self.read_config()?.mcp_servers.clone()
            }
        };
        let mut out = Vec::with_capacity(servers.len());
        for config in &servers {
            let snapshot = self.mcp_manager.client_snapshot(&config.name).await;
            let (connected, tool_count, last_error, diagnostic) = match snapshot {
                Some(snapshot) => {
                    let status = snapshot.status;
                    let connected = matches!(&status, McpClientStatus::Connected);
                    let error = match status {
                        McpClientStatus::Offline { error } => error,
                        _ => snapshot.last_error.unwrap_or_default(),
                    };
                    (
                        connected,
                        self.mcp_manager.cached_tool_count(&config.name).await,
                        sanitize_diagnostic(&error),
                        snapshot
                            .diagnostic
                            .map(|text| sanitize_diagnostic(&text))
                            .or_else(|| {
                                (config.enabled && !connected)
                                    .then(|| haven_mcp::MCP_TOOLS_NOT_DISCOVERED_DIAGNOSTIC.into())
                            }),
                    )
                }
                None => (
                    false,
                    None,
                    String::new(),
                    config
                        .enabled
                        .then(|| haven_mcp::MCP_TOOLS_NOT_DISCOVERED_DIAGNOSTIC.to_string()),
                ),
            };
            out.push(McpStatusOutput {
                name: config.name.clone(),
                enabled: config.enabled,
                connected,
                tools: tool_count,
                last_error,
                diagnostic,
            });
        }
        Ok(out)
    }

    pub(crate) async fn mcp_connect(&self, name: &str) -> Result<McpConnectionOutput> {
        let _config_apply_guard = self.lock_config_apply().await;
        let config = self
            .server_configs
            .read()
            .await
            .get(name)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("MCP server '{}' not found in config", name))?;
        if !config.enabled {
            anyhow::bail!("MCP server '{}' is disabled", name);
        }
        self.mcp_manager.remove_client(name).await;
        self.mcp_manager.connect_server(&config).await?;
        Ok(McpConnectionOutput {
            name: name.to_string(),
            connected: true,
        })
    }

    pub(crate) async fn mcp_disconnect(&self, name: &str) -> Result<McpConnectionOutput> {
        let _config_apply_guard = self.lock_config_apply().await;
        self.mcp_manager.remove_client(name).await;
        Ok(McpConnectionOutput {
            name: name.to_string(),
            connected: false,
        })
    }

    pub(crate) async fn mcp_add(&self, fields: &McpAddFields) -> Result<McpAddOutput> {
        let _config_apply_guard = self.lock_config_apply().await;
        let command = fields.command.clone().filter(|value| !value.is_empty());
        let url = fields.url.clone().filter(|value| !value.is_empty());
        match fields.transport {
            McpTransportType::Stdio if command.is_none() => {
                anyhow::bail!("command is required (the binary to spawn) for stdio servers")
            }
            McpTransportType::Http if url.is_none() => {
                anyhow::bail!("url is required (the HTTP endpoint) for http servers")
            }
            _ => {}
        }
        let config = McpServerConfig {
            name: fields.name.clone(),
            transport: fields.transport.clone(),
            command: command.unwrap_or_default(),
            args: fields.args.clone(),
            env: fields.env.clone(),
            env_refs: Vec::new(),
            cwd: fields.cwd.clone().filter(|value| !value.is_empty()),
            url: url.unwrap_or_default(),
            enabled: fields.enabled,
        };
        if let Some(existing) = self
            .read_config()?
            .mcp_servers
            .iter()
            .find(|server| server.name == fields.name)
            .cloned()
        {
            return self
                .apply_mcp_config_update(&existing, &config, fields.auto_connect)
                .await
                .map(|updated| McpAddOutput {
                    name: updated.name,
                    enabled: updated.enabled,
                    saved: updated.saved,
                    connected: updated.connected,
                    warning: None,
                });
        }
        let mut servers = self.config_service()?.snapshot()?.config.mcp_servers;
        servers.push(config.clone());
        self.config_service()?
            .apply_patch(ConfigPatch::McpServers(servers))?;
        self.server_configs
            .write()
            .await
            .insert(config.name.clone(), config.clone());
        self.mcp_manager.invalidate_catalog();

        let (connected, warning) = if config.enabled && fields.auto_connect {
            match self.mcp_manager.connect_server(&config).await {
                Ok(()) => (true, None),
                Err(error) => (
                    false,
                    Some(format!(
                        "config saved but connect failed: {}",
                        sanitize_diagnostic(&error.to_string())
                    )),
                ),
            }
        } else {
            (false, None)
        };
        self.rebuild_catalog().await?;
        Ok(McpAddOutput {
            name: config.name,
            enabled: config.enabled,
            saved: true,
            connected,
            warning,
        })
    }

    pub(crate) async fn mcp_update(
        &self,
        fields: &McpUpdateFields,
    ) -> Result<McpConfigUpdateOutput> {
        let _config_apply_guard = self.lock_config_apply().await;
        let existing = self
            .read_config()?
            .mcp_servers
            .iter()
            .find(|server| server.name == fields.name)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("MCP server '{}' not found in config", fields.name))?;
        let mut updated = existing.clone();
        if let Some(command) = fields.command.as_deref().filter(|value| !value.is_empty()) {
            updated.command = command.to_string();
        }
        if let Some(args) = &fields.args {
            updated.args = args.clone();
        }
        if let Some(env) = &fields.env {
            updated.env = env.clone();
        }
        if let Some(cwd) = &fields.cwd {
            updated.cwd = Some(cwd.clone());
        }
        if let Some(url) = &fields.url {
            updated.url = url.clone();
        }
        if let Some(transport) = &fields.transport {
            updated.transport = transport.clone();
        }
        if let Some(enabled) = fields.enabled {
            updated.enabled = enabled;
        }
        self.apply_mcp_config_update(&existing, &updated, true)
            .await
    }

    pub(crate) async fn mcp_toggle(
        &self,
        name: &str,
        enabled: bool,
    ) -> Result<McpConfigUpdateOutput> {
        let _config_apply_guard = self.lock_config_apply().await;
        let existing = self
            .read_config()?
            .mcp_servers
            .iter()
            .find(|server| server.name == name)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("MCP server '{}' not found in config", name))?;
        let mut updated = existing.clone();
        updated.enabled = enabled;
        self.apply_mcp_config_update(&existing, &updated, true)
            .await
    }

    pub(crate) async fn mcp_remove(&self, name: &str) -> Result<McpRemoveOutput> {
        let _config_apply_guard = self.lock_config_apply().await;
        let mut servers = self.config_service()?.snapshot()?.config.mcp_servers;
        let before = servers.len();
        servers.retain(|server| server.name != name);
        if servers.len() == before {
            anyhow::bail!("MCP server '{}' not found in config", name);
        }
        self.config_service()?
            .apply_patch(ConfigPatch::McpServers(servers))?;
        self.mcp_manager.remove_client(name).await;
        self.server_configs.write().await.remove(name);
        self.mcp_manager.invalidate_catalog();
        self.rebuild_catalog().await?;
        Ok(McpRemoveOutput {
            name: name.to_string(),
            removed: true,
            connected: false,
        })
    }

    pub(crate) async fn mcp_reload(&self) -> Result<McpReloadOutput> {
        let _config_apply_guard = self.lock_config_apply().await;
        let servers = self.read_config()?.mcp_servers.clone();
        let mut map = self.server_configs.write().await;
        map.clear();
        for server in &servers {
            map.insert(server.name.clone(), server.clone());
        }
        drop(map);
        self.mcp_manager.invalidate_catalog();
        for name in self.mcp_manager.list_clients().await {
            self.mcp_manager.remove_client(&name).await;
        }
        let mut connected = Vec::new();
        for server in &servers {
            if !server.enabled {
                continue;
            }
            match self.mcp_manager.connect_server(server).await {
                Ok(()) => connected.push(McpReloadConnectionOutput::Connected {
                    name: server.name.clone(),
                    connected: true,
                }),
                Err(error) => connected.push(McpReloadConnectionOutput::Failed {
                    name: server.name.clone(),
                    connected: false,
                    error: sanitize_diagnostic(&error.to_string()),
                }),
            }
        }
        self.rebuild_catalog().await?;
        Ok(McpReloadOutput {
            reloaded: true,
            connected,
        })
    }

    /// Execute one already-authorized renderer reconnect. Authorization waits
    /// happen before this method; the shared config gate then protects the
    /// final generation/target check and the complete reconnect + monitor
    /// restart.
    pub(crate) async fn mcp_reconnect(
        &self,
        name: &str,
        authorized_config_version: ConfigVersion,
    ) -> std::result::Result<McpConnectionOutput, NativeMcpServiceError> {
        let _config_apply_guard = self.lock_config_apply().await;
        let snapshot = self
            .config_service()
            .and_then(|service| service.snapshot())
            .map_err(NativeMcpServiceError::BeforeSideEffect)?;
        if snapshot.version != authorized_config_version {
            return Err(NativeMcpServiceError::Preflight(
                "MCP reconnect authorization is stale; refresh and try again".into(),
            ));
        }
        let configured = snapshot
            .config
            .mcp_servers
            .iter()
            .find(|server| server.name == name)
            .filter(|server| server.enabled)
            .ok_or_else(|| {
                NativeMcpServiceError::Preflight(format!(
                    "MCP server '{}' is not enabled in config",
                    name
                ))
            })?;
        if !self.mcp_manager.has_client(name).await {
            return Err(NativeMcpServiceError::Preflight(format!(
                "MCP client '{}' is no longer connected",
                name
            )));
        }

        // Confirm that the live client still represents this configured
        // server before reconnecting it. The config version check above also
        // invalidates any pending confirmation after an edit.
        if !self
            .mcp_manager
            .client_matches_config(name, configured)
            .await
        {
            return Err(NativeMcpServiceError::Preflight(format!(
                "MCP server '{}' changed after authorization",
                name
            )));
        }
        self.mcp_manager
            .reconnect(name)
            .await
            .map_err(NativeMcpServiceError::SideEffect)?;

        Ok(McpConnectionOutput {
            name: name.to_string(),
            connected: true,
        })
    }

    /// Reconcile only the target set captured in the authorization request.
    /// A stale config version or changed live diff is rejected before any
    /// client is connected or disconnected.
    pub(crate) async fn mcp_refresh(
        &self,
        authorized_plan: &McpRefreshPlan,
    ) -> std::result::Result<McpRefreshOutput, NativeMcpServiceError> {
        let _config_apply_guard = self.lock_config_apply().await;
        let snapshot = self
            .config_service()
            .and_then(|service| service.snapshot())
            .map_err(NativeMcpServiceError::BeforeSideEffect)?;
        if snapshot.version != authorized_plan.config_version {
            return Err(NativeMcpServiceError::Preflight(
                "MCP refresh authorization is stale; refresh and try again".into(),
            ));
        }
        let servers = snapshot.config.mcp_servers;
        let reconcile = self.mcp_manager.reconcile_servers(&servers).await;
        let current_plan = McpRefreshPlan::from_reconcile(snapshot.version, &reconcile);
        if current_plan != *authorized_plan {
            return Err(NativeMcpServiceError::Preflight(
                "MCP refresh targets changed after authorization; refresh and try again".into(),
            ));
        }

        // Keep the in-memory config index aligned even when the authorized
        // diff contains no connection changes.
        {
            let mut map = self.server_configs.write().await;
            map.clear();
            for server in &servers {
                map.insert(server.name.clone(), server.clone());
            }
        }

        // Remove stale generations first, then connect new and changed
        // servers in the same order as the prior renderer refresh path.
        let changed = reconcile.to_connect_changed;
        let mut updated = Vec::new();
        for server in &changed {
            tracing::info!(server = %server.name, "MCP config changed; reconnecting authorized target");
            self.mcp_manager.remove_client(&server.name).await;
            updated.push(server.name.clone());
        }

        let mut added = Vec::new();
        let mut failed = Vec::new();
        for server in reconcile.to_connect_new.into_iter().chain(changed) {
            match self.mcp_manager.connect_server(&server).await {
                Ok(()) => {
                    if !updated.iter().any(|name| name == &server.name) {
                        added.push(server.name.clone());
                    }
                }
                Err(error) => {
                    tracing::warn!(
                        server = %server.name,
                        error = %sanitize_diagnostic(&error.to_string()),
                        "authorized MCP refresh target failed to connect"
                    );
                    failed.push(server.name.clone());
                }
            }
        }

        let mut removed = Vec::new();
        for name in reconcile.to_remove {
            self.mcp_manager.remove_client(&name).await;
            removed.push(name);
        }
        self.rebuild_catalog()
            .await
            .map_err(NativeMcpServiceError::SideEffect)?;
        Ok(McpRefreshOutput {
            added,
            removed,
            updated,
            failed,
        })
    }

    async fn apply_mcp_config_update(
        &self,
        old_config: &McpServerConfig,
        new_config: &McpServerConfig,
        connect_if_enabled: bool,
    ) -> Result<McpConfigUpdateOutput> {
        let name = new_config.name.clone();
        let config_changed = old_config.transport != new_config.transport
            || old_config.command != new_config.command
            || old_config.args != new_config.args
            || old_config.env != new_config.env
            || old_config.url != new_config.url;
        let will_enable = new_config.enabled && !old_config.enabled;
        if new_config.enabled && connect_if_enabled && (will_enable || config_changed) {
            self.mcp_manager.remove_client(&name).await;
            if let Err(error) = self.mcp_manager.connect_server(new_config).await {
                if old_config.enabled
                    && let Err(rollback) = self.mcp_manager.connect_server(old_config).await
                {
                    tracing::error!(
                        server = name,
                        error = %haven_common::error::sanitize_error_text(&rollback.to_string()),
                        "MCP reconnect rollback failed after config update failure"
                    );
                }
                anyhow::bail!(
                    "MCP server '{}' not connected; config left unchanged: {}",
                    name,
                    sanitize_diagnostic(&error.to_string())
                );
            }
        } else if !new_config.enabled || config_changed {
            self.mcp_manager.remove_client(&name).await;
        }

        let persist_result = (|| -> Result<()> {
            let mut servers = self.config_service()?.snapshot()?.config.mcp_servers;
            let Some(existing) = servers.iter_mut().find(|server| server.name == name) else {
                anyhow::bail!("MCP server '{}' not found in config", name);
            };
            *existing = new_config.clone();
            self.config_service()?
                .apply_patch(ConfigPatch::McpServers(servers))?;
            Ok(())
        })();
        if let Err(error) = persist_result {
            self.mcp_manager.remove_client(&name).await;
            if old_config.enabled
                && let Err(rollback) = self.mcp_manager.connect_server(old_config).await
            {
                tracing::error!(
                    server = name,
                    error = %haven_common::error::sanitize_error_text(&rollback.to_string()),
                    "MCP reconnect rollback failed after config persistence error"
                );
            }
            return Err(error);
        }
        self.server_configs
            .write()
            .await
            .insert(name.clone(), new_config.clone());
        self.mcp_manager.invalidate_catalog();
        self.rebuild_catalog().await?;
        Ok(McpConfigUpdateOutput {
            name: name.clone(),
            enabled: new_config.enabled,
            saved: true,
            connected: self.mcp_manager.has_client(&name).await,
        })
    }
}

async fn collect_diagnostic_model_health<F, Fut>(
    models: impl IntoIterator<Item = (RequestKind, bool)>,
    timeout: Duration,
    check: F,
) -> BTreeMap<String, DiagnosticModelStatus>
where
    F: Fn(RequestKind) -> Fut + Sync,
    Fut: Future<Output = Result<(), String>> + Send,
{
    futures_util::future::join_all(models.into_iter().map(|(request, configured)| {
        let check = &check;
        async move {
            let status = if !configured {
                "not_configured".to_string()
            } else {
                match tokio::time::timeout(timeout, check(request)).await {
                    Ok(Ok(())) => "ok".to_string(),
                    Ok(Err(error)) => format!("error: {}", sanitize_diagnostic(&error)),
                    Err(_) => format!("error: health check timed out after {timeout:?}"),
                }
            };
            (
                request.as_str().to_string(),
                DiagnosticModelStatus {
                    configured,
                    status,
                    route_issue: None,
                    model_config_id: None,
                    provider: None,
                    provider_model_id: None,
                },
            )
        }
    }))
    .await
    .into_iter()
    .collect()
}

struct DiagnosticRouteMetadata {
    configured: bool,
    route_issue: Option<&'static str>,
    model_config_id: Option<String>,
    provider: Option<String>,
    provider_model_id: Option<String>,
}

fn diagnostic_route_metadata(
    config: &RouterConfig,
    request: RequestKind,
) -> DiagnosticRouteMetadata {
    let Some(policy) = config.policy(request) else {
        return DiagnosticRouteMetadata {
            configured: false,
            route_issue: Some("missing_request_policy"),
            model_config_id: None,
            provider: None,
            provider_model_id: None,
        };
    };
    let Some(model) = config.model(&policy.primary) else {
        return DiagnosticRouteMetadata {
            configured: false,
            route_issue: Some("missing_model_config"),
            model_config_id: Some(policy.primary.clone()),
            provider: None,
            provider_model_id: None,
        };
    };
    let model_config_id = Some(model.id.clone());
    let provider =
        (!model.endpoint.provider.trim().is_empty()).then(|| model.endpoint.provider.clone());
    let provider_model_id =
        (!model.endpoint.model_name.trim().is_empty()).then(|| model.endpoint.model_name.clone());
    let issue = if provider.is_none() || provider_model_id.is_none() {
        Some("model_assignment_incomplete")
    } else if !model.capabilities.contains(&request.required_capability()) {
        Some("required_capability_not_declared")
    } else if !endpoint_credentials_ready(&model.endpoint) {
        Some("provider_credentials_missing")
    } else {
        None
    };
    DiagnosticRouteMetadata {
        configured: issue.is_none(),
        route_issue: issue,
        model_config_id,
        provider,
        provider_model_id,
    }
}

pub(crate) fn sanitize_log_line(line: &str) -> String {
    const SENSITIVE_MARKERS: &[&str] = &[
        "api_key",
        "access_token",
        "authorization",
        "password",
        "secret",
        "prompt",
        "transcript",
        "message",
        "content",
        "command",
        "stdout",
        "stderr",
    ];
    let lower = line.to_ascii_lowercase();
    if SENSITIVE_MARKERS
        .iter()
        .any(|marker| lower.contains(marker))
    {
        return "[redacted diagnostic line]".into();
    }
    let mut bounded = line.to_string();
    if bounded.chars().count() > 512 {
        let cut = bounded.floor_char_boundary(512);
        bounded.truncate(cut);
        bounded.push('…');
    }
    bounded
}

pub(crate) fn sanitize_diagnostic(text: &str) -> String {
    sanitize_log_line(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use haven_common::config::{Capability, ModelEndpoint, RequestPolicy, RoutedModel};
    use tokio::sync::Barrier;

    #[test]
    fn diagnostic_route_explains_missing_policy_and_keeps_model_identities_distinct() {
        let mut config = RouterConfig::default();
        let missing_policy = diagnostic_route_metadata(&config, RequestKind::Vision);
        assert!(!missing_policy.configured);
        assert_eq!(missing_policy.route_issue, Some("missing_request_policy"));

        config.request_policies.push(RequestPolicy {
            request: RequestKind::Vision,
            primary: "vision-profile".into(),
        });
        let missing_model = diagnostic_route_metadata(&config, RequestKind::Vision);
        assert_eq!(missing_model.route_issue, Some("missing_model_config"));
        assert_eq!(
            missing_model.model_config_id.as_deref(),
            Some("vision-profile")
        );

        config.models.push(RoutedModel {
            id: "vision-profile".into(),
            endpoint: ModelEndpoint {
                provider: "deepseek".into(),
                model_name: "deepseek-flash".into(),
                api_key: "test-credential".into(),
                ..Default::default()
            },
            capabilities: vec![Capability::Vision],
        });
        let configured = diagnostic_route_metadata(&config, RequestKind::Vision);
        assert!(configured.configured);
        assert_eq!(
            configured.model_config_id.as_deref(),
            Some("vision-profile")
        );
        assert_eq!(
            configured.provider_model_id.as_deref(),
            Some("deepseek-flash")
        );
    }

    #[tokio::test]
    async fn diagnostic_model_checks_run_concurrently_and_keep_unconfigured_routes() {
        let barrier = Arc::new(Barrier::new(2));
        let check_barrier = Arc::clone(&barrier);
        let models = collect_diagnostic_model_health(
            [
                (RequestKind::Chat, true),
                (RequestKind::FastChat, true),
                (RequestKind::Vision, false),
            ],
            Duration::from_millis(250),
            move |_| {
                let barrier = Arc::clone(&check_barrier);
                async move {
                    barrier.wait().await;
                    Ok(())
                }
            },
        )
        .await;

        assert_eq!(models.len(), 3);
        assert_eq!(models["chat"].status, "ok");
        assert_eq!(models["fast_chat"].status, "ok");
        assert_eq!(models["vision"].status, "not_configured");
    }

    #[tokio::test]
    async fn diagnostic_model_health_timeout_is_reported_per_route() {
        let models = collect_diagnostic_model_health(
            [(RequestKind::Chat, true)],
            Duration::from_millis(10),
            |_| async {
                tokio::time::sleep(Duration::from_millis(100)).await;
                Ok(())
            },
        )
        .await;

        assert_eq!(
            models["chat"].status,
            "error: health check timed out after 10ms"
        );
    }
}
