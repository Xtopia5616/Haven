//! Domain services shared by the five typed administration operations.
//!
//! This module deliberately contains no operation selector. Each public method
//! accepts the fields for exactly one operation; the typed operation wrappers in
//! `admin.rs` own selection, schema, policy, and error conversion.

use super::{AdminContext, McpAddFields, McpRefreshPlan, McpUpdateFields};
use crate::ToolRegistry;
use anyhow::{Error, Result};
use haven_common::config::{
    AppConfig, ConfigLoader, ConfigPatch, LogConfig, LogLevel, McpServerConfig, RequestKind,
    Settings,
};
use haven_common::types::McpTransportType;
use haven_mcp::{McpClientStatus, McpStatusChangeEvent};
use serde::Serialize;
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::RwLock;

use haven_mcp::McpManager;
use haven_skills::SkillsEngine;

pub(crate) enum NativeMcpServiceError {
    Preflight(String),
    BeforeSideEffect(Error),
    SideEffect(Error),
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct LogsLevelResult {
    pub(crate) level: LogLevel,
    pub(crate) saved: bool,
    pub(crate) version: u64,
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
    pub(crate) skills: Vec<DiagnosticSkillOutput>,
    pub(crate) sessions: DiagnosticSessionsOutput,
    pub(crate) log: DiagnosticLogOutput,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct DiagnosticModelStatus {
    pub(crate) configured: bool,
    pub(crate) status: String,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct DiagnosticToolsStatus {
    pub(crate) count: usize,
    pub(crate) names: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct DiagnosticSkillOutput {
    pub(crate) name: String,
    pub(crate) enabled: bool,
    pub(crate) description: String,
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
    pub(crate) name: String,
    pub(crate) enabled: bool,
    pub(crate) description: String,
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
    pub(crate) tools: usize,
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
    pub(crate) skills_engine: SkillsEngine,
    pub(crate) mcp_manager: Arc<McpManager>,
    pub(crate) server_configs: Arc<RwLock<HashMap<String, McpServerConfig>>>,
    pub(crate) registry: ToolRegistry,
    pub(crate) max_instructions_bytes: usize,
    pub(crate) max_script_bytes: usize,
}

impl AdminServices {
    pub(crate) fn new(
        context: AdminContext,
        skills_engine: SkillsEngine,
        mcp_manager: Arc<McpManager>,
        server_configs: Arc<RwLock<HashMap<String, McpServerConfig>>>,
        registry: ToolRegistry,
        max_instructions_bytes: usize,
        max_script_bytes: usize,
    ) -> Self {
        Self {
            context,
            skills_engine,
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
            version: update.snapshot.version,
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
            let mut health = BTreeMap::new();
            for request in RequestKind::ALL {
                let configured = router.is_request_configured(*request).await;
                let status = if !configured {
                    "not_configured".to_string()
                } else {
                    match router
                        .health_check(haven_llm::types::HealthCheckRequest { request: *request })
                        .await
                    {
                        Ok(()) => "ok".to_string(),
                        Err(error) => {
                            format!("error: {}", sanitize_diagnostic(&error.to_string()))
                        }
                    }
                };
                health.insert(
                    request.as_str().to_string(),
                    DiagnosticModelStatus { configured, status },
                );
            }
            Some(health)
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

        let skills: Vec<DiagnosticSkillOutput> = self
            .skills_engine
            .list()
            .await
            .into_iter()
            .map(|skill| DiagnosticSkillOutput {
                name: skill.name,
                enabled: skill.enabled,
                description: skill.description,
            })
            .collect();

        let sessions = if let Some(session_store) = &self.context.session_store {
            let mut counts: BTreeMap<String, usize> = BTreeMap::new();
            match session_store.list_history(50, 0).await {
                Ok(sessions) => {
                    for session in &sessions {
                        *counts
                            .entry(session.status.as_str().to_string())
                            .or_default() += 1;
                    }
                }
                Err(error) => {
                    tracing::warn!(error = %error, "admin diagnostics list_sessions failed")
                }
            }
            let total = match session_store.count_history().await {
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

        let log_path = self
            .context
            .log_path
            .as_ref()
            .map(|path| path.to_string_lossy().to_string())
            .unwrap_or_else(|| LogConfig::default_log_path().to_string_lossy().to_string());
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
        let path = self
            .context
            .log_path
            .clone()
            .unwrap_or_else(LogConfig::default_log_path);
        let content = match tokio::fs::read(&path).await {
            Ok(bytes) => haven_common::encoding::decode_lossy(&bytes),
            Err(error) => {
                return Ok(LogsTailOutput::Unavailable {
                    path: path.to_string_lossy().to_string(),
                    error: format!("cannot read log file: {error}"),
                });
            }
        };
        let lines: Vec<&str> = content.lines().collect();
        let total_lines = lines.len();
        let start = lines.len().saturating_sub(limit);
        let lines: Vec<String> = lines[start..]
            .iter()
            .map(|line| sanitize_log_line(line))
            .collect();
        Ok(LogsTailOutput::Read {
            path: path.to_string_lossy().to_string(),
            total_lines,
            lines,
        })
    }

    pub(crate) async fn sessions(&self, limit: Option<i64>) -> Result<SessionsOutput> {
        let Some(session_store) = &self.context.session_store else {
            return Ok(SessionsOutput::Unavailable { unavailable: true });
        };
        let sessions = session_store
            .list_history(limit.unwrap_or(10).clamp(1, 50), 0)
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
            .list_history(limit.unwrap_or(10).clamp(1, 50), 0)
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
            .skills_engine
            .list()
            .await
            .into_iter()
            .map(|skill| SkillSummaryOutput {
                name: skill.name,
                enabled: skill.enabled,
                description: skill.description,
                root: skill.root,
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
        if self.skills_engine.get_skill(name).await.is_none() {
            anyhow::bail!("skill '{}' not found", name);
        }
        self.skills_engine.set_enabled(name, enabled).await?;
        let filter = self.skills_engine.enabled_filter().await;
        let snapshot = config_service.snapshot()?;
        let mut skills = snapshot.config.skills;
        skills.enabled = filter;
        if let Err(error) = config_service.apply_patch(ConfigPatch::Skills {
            config: skills,
            exec: snapshot.config.skills_exec,
        }) {
            if let Err(rollback) = self.skills_engine.set_enabled(name, !enabled).await {
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
        validate_skill_name(name)?;
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

        let root = self.skills_engine.resolved_root().await;
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

        let mut has_script = false;
        if let Some(script) = script {
            let scripts = skill_dir.join("scripts");
            tokio::fs::create_dir_all(&scripts).await?;
            tokio::fs::write(scripts.join("main.py"), script).await?;
            has_script = true;
        }

        self.skills_engine.refresh_from_disk().await?;
        self.skills_engine.set_enabled(name, true).await?;
        let filter = self.skills_engine.enabled_filter().await;
        let snapshot = config_service.snapshot()?;
        let mut skills = snapshot.config.skills;
        skills.enabled = filter;
        if let Err(error) = config_service.apply_patch(ConfigPatch::Skills {
            config: skills,
            exec: snapshot.config.skills_exec,
        }) {
            if let Err(rollback) = tokio::fs::remove_dir_all(&skill_dir).await {
                tracing::warn!(path = %skill_dir.display(), error = %rollback, "skill creation rollback failed");
            }
            if let Err(refresh_error) = self.skills_engine.refresh_from_disk().await {
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
            let client = self.mcp_manager.get_client(&config.name).await;
            let (connected, tool_count, last_error, diagnostic) = match &client {
                Some(client) => {
                    let status = client.status().await;
                    let connected = matches!(status, McpClientStatus::Connected);
                    let error = match status {
                        McpClientStatus::Offline { error } => error,
                        _ => client.last_error().await.unwrap_or_default(),
                    };
                    (
                        connected,
                        client.tools_cache().await.len(),
                        sanitize_diagnostic(&error),
                        client
                            .diagnostic()
                            .await
                            .map(|text| sanitize_diagnostic(&text)),
                    )
                }
                None => (false, 0, String::new(), None),
            };
            out.push(McpStatusOutput {
                name: config.name,
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
        authorized_version: u64,
    ) -> std::result::Result<McpConnectionOutput, NativeMcpServiceError> {
        let _config_apply_guard = self.lock_config_apply().await;
        let snapshot = self
            .config_service()
            .and_then(|service| service.snapshot())
            .map_err(NativeMcpServiceError::BeforeSideEffect)?;
        if snapshot.version != authorized_version {
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
        let Some(client) = self.mcp_manager.get_client(name).await else {
            return Err(NativeMcpServiceError::Preflight(format!(
                "MCP client '{}' is no longer connected",
                name
            )));
        };

        // Confirm that the live client still represents this configured
        // server before reconnecting it. The config version check above also
        // invalidates any pending confirmation after an edit.
        if !client.matches_config(configured) {
            return Err(NativeMcpServiceError::Preflight(format!(
                "MCP server '{}' changed after authorization",
                name
            )));
        }
        self.mcp_manager
            .reconnect(name)
            .await
            .map_err(NativeMcpServiceError::SideEffect)?;

        let discovery = snapshot.config.mcp_discovery;
        client.spawn_monitor(
            std::time::Duration::from_secs(discovery.health_interval_secs),
            std::time::Duration::from_millis(discovery.reconnect_initial_ms),
            std::time::Duration::from_millis(discovery.reconnect_max_ms),
            discovery.reconnect_max_retries,
            self.mcp_manager.status_tx(),
        );
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
            let _ = self.mcp_manager.status_tx().send(McpStatusChangeEvent {
                name: server.name.clone(),
                status: McpClientStatus::Disconnected,
            });
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
            let _ = self.mcp_manager.status_tx().send(McpStatusChangeEvent {
                name: name.clone(),
                status: McpClientStatus::Disconnected,
            });
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
            connected: self.mcp_manager.get_client(&name).await.is_some(),
        })
    }
}

pub(crate) fn validate_skill_name(name: &str) -> Result<()> {
    let valid = !name.is_empty()
        && name.len() <= 128
        && name.chars().all(|character| {
            character.is_ascii_alphanumeric() || character == '-' || character == '_'
        });
    if !valid {
        anyhow::bail!(
            "invalid skill name '{}': use 1-128 characters of a-z, A-Z, 0-9, '-' or '_'",
            name
        );
    }
    Ok(())
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
