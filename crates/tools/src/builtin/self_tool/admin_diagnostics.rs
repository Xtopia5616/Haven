use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::Result;
use haven_common::config::{ConfigPatch, LogConfig, LogLevel, Settings};
use haven_llm::EndpointRole;
use serde_json::Value;

use super::super::admin_support::mask_sensitive_config;
use super::{SelfParams, SelfTool};

impl SelfTool {
    pub(super) async fn op_status(&self) -> Result<Value> {
        let mut out = serde_json::json!({});

        // Config overview (API keys masked via Settings).
        match self.read_config() {
            Ok(config) => {
                out["config_path"] = self.config_path()?.to_string_lossy().to_string().into();
                let mut settings = serde_json::to_value(Settings::from(&config))?;
                mask_sensitive_config(&mut settings);
                out["settings"] = settings;
            }
            Err(e) => {
                out["config_error"] = sanitize_diagnostic(&e.to_string()).into();
            }
        }

        // Model endpoint health.
        if let Some(router) = &self.context.router {
            let mut health = serde_json::Map::new();
            for role in EndpointRole::ALL {
                let configured = router.is_role_configured(*role).await;
                let status = if !configured {
                    "not_configured".to_string()
                } else {
                    match router.health_check(*role).await {
                        Ok(()) => "ok".to_string(),
                        Err(e) => format!("error: {}", sanitize_diagnostic(&e.to_string())),
                    }
                };
                health.insert(
                    role.as_str().to_string(),
                    serde_json::json!({ "configured": configured, "status": status }),
                );
            }
            out["models"] = Value::Object(health);
        }

        // Registered global tools.
        let schemas = self.registry.list_schemas().await;
        let names: Vec<Value> = schemas.iter().map(|s| s["name"].clone()).collect();
        out["tools"] = serde_json::json!({ "count": schemas.len(), "names": names });

        // MCP servers.
        match self.mcp_status().await {
            Ok(mcp) => out["mcp"] = mcp,
            Err(error) => {
                out["mcp_error"] = sanitize_diagnostic(&error.to_string()).into();
            }
        }

        // Skills.
        let skills: Vec<Value> = self
            .skills_engine
            .list()
            .await
            .into_iter()
            .map(|s| {
                serde_json::json!({
                    "name": s.name,
                    "enabled": s.enabled,
                    "description": s.description,
                })
            })
            .collect();
        out["skills"] = Value::Array(skills);

        // Recent session counts.
        if let Some(db) = &self.context.db {
            let mut counts: HashMap<String, usize> = HashMap::new();
            match db.list_sessions(50, 0) {
                Ok(sessions) => {
                    for t in &sessions {
                        *counts.entry(t.status.clone()).or_default() += 1;
                    }
                }
                Err(e) => tracing::warn!("self status: list_sessions failed: {}", e),
            }
            let total = match db.count_sessions() {
                Ok(n) => n,
                Err(e) => {
                    tracing::warn!("self status: count_sessions failed: {}", e);
                    0
                }
            };
            out["sessions"] = serde_json::json!({ "total": total, "recent_50_by_status": counts });
        } else {
            out["sessions"] = serde_json::json!({ "unavailable": true });
        }

        let log_path = self
            .context
            .log_path
            .as_ref()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|| LogConfigDefaultPath::path().to_string_lossy().to_string());
        out["log"] = serde_json::json!({ "path": log_path });

        Ok(out)
    }

    pub(super) async fn op_logs_tail(&self, params: &SelfParams) -> Result<Value> {
        let limit = params.limit.unwrap_or(50).clamp(1, 500) as usize;
        let path = self
            .context
            .log_path
            .clone()
            .unwrap_or_else(LogConfigDefaultPath::path);
        let content = match tokio::fs::read(&path).await {
            Ok(b) => haven_common::encoding::decode_lossy(&b),
            Err(e) => {
                return Ok(serde_json::json!({
                    "path": path.to_string_lossy(),
                    "error": format!("cannot read log file: {e}"),
                }));
            }
        };
        let lines: Vec<&str> = content.lines().collect();
        let total_lines = lines.len();
        let start = lines.len().saturating_sub(limit);
        let lines: Vec<String> = lines[start..]
            .iter()
            .map(|line| sanitize_log_line(line))
            .collect();
        Ok(serde_json::json!({
            "path": path.to_string_lossy(),
            "total_lines": total_lines,
            "lines": lines,
        }))
    }

    pub(super) async fn op_logs_level(&self, params: &SelfParams) -> Result<Value> {
        let level = params
            .level
            .as_deref()
            .map(str::to_lowercase)
            .filter(|l| matches!(l.as_str(), "trace" | "debug" | "info" | "warn" | "error"))
            .ok_or_else(|| {
                anyhow::anyhow!("level must be one of: trace, debug, info, warn, error")
            })?;
        let parsed = match level.as_str() {
            "trace" => LogLevel::Trace,
            "debug" => LogLevel::Debug,
            "warn" => LogLevel::Warn,
            "error" => LogLevel::Error,
            _ => LogLevel::Info,
        };
        self.config_service()?
            .apply_patch(ConfigPatch::LogLevel(parsed))?;
        if let Some(f) = &self.context.set_log_level {
            f(level.clone());
        }
        Ok(serde_json::json!({ "level": level, "saved": true }))
    }

    fn list_actions_for_op(
        &self,
        params: &SelfParams,
    ) -> Result<Option<(i64, Vec<haven_memory::repositories::sessions::Session>)>> {
        let limit = params.limit.unwrap_or(10).clamp(1, 50);
        let Some(db) = &self.context.db else {
            return Ok(None);
        };
        let sessions = db.list_sessions(limit, 0)?;
        Ok(Some((limit, sessions)))
    }

    pub(super) async fn op_sessions(&self, params: &SelfParams) -> Result<Value> {
        let Some((_limit, sessions)) = self.list_actions_for_op(params)? else {
            return Ok(serde_json::json!({ "unavailable": true }));
        };
        let rows: Vec<Value> = sessions
            .into_iter()
            .map(|t| {
                serde_json::json!({
                    "id": t.id,
                    "status": t.status,
                    "title": t.title,
                    "input_chars": t.input_text.chars().count(),
                    "created_at": t.created_at,
                    "updated_at": t.updated_at,
                })
            })
            .collect();
        Ok(serde_json::json!({ "sessions": rows }))
    }

    pub(super) async fn op_errors(&self, params: &SelfParams) -> Result<Value> {
        let Some((_limit, sessions)) = self.list_actions_for_op(params)? else {
            return Ok(serde_json::json!({ "unavailable": true }));
        };
        let rows: Vec<Value> = sessions
            .into_iter()
            .filter(|t| t.status == "error")
            .map(|t| {
                serde_json::json!({
                    "id": t.id,
                    "title": t.title,
                    "input_chars": t.input_text.chars().count(),
                    "created_at": t.created_at,
                    "transcript_chars": t.transcript.chars().count(),
                })
            })
            .collect();
        Ok(serde_json::json!({ "errors": rows }))
    }
}

/// Wrapper so `unwrap_or_else(LogConfigDefaultPath::path)` stays explicit in
/// the diagnostics domain instead of leaking a closure detail into callers.
pub(super) struct LogConfigDefaultPath;

impl LogConfigDefaultPath {
    pub(super) fn path() -> PathBuf {
        LogConfig::default_log_path()
    }
}

/// Keep diagnostics useful without turning a model-visible log tail into a
/// prompt, command-output, or credential exfiltration channel.
pub(super) fn sanitize_log_line(line: &str) -> String {
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
