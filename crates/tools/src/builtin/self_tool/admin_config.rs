use std::path::PathBuf;

use anyhow::Result;
use haven_common::config::{AppConfig, ConfigLoader, ConfigPatch, ConfigService, Settings};
use serde_json::Value;

use super::super::admin_support::{mask_sensitive_config, value_at};
use super::{SelfParams, SelfTool};

impl SelfTool {
    /// Read the current config from the shared service, or directly from the
    /// default file for headless operation.
    pub(super) fn read_config(&self) -> Result<AppConfig> {
        match &self.context.config_service {
            Some(service) => Ok(service.snapshot()?.config),
            None => Ok(ConfigLoader::load()?.config().clone()),
        }
    }

    pub(super) fn config_path(&self) -> Result<PathBuf> {
        match &self.context.config_service {
            Some(service) => Ok(service.path()?),
            None => Ok(ConfigLoader::default_path()),
        }
    }

    pub(super) fn config_service(&self) -> Result<&ConfigService> {
        self.context
            .config_service
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("configuration administration is unavailable"))
    }

    pub(super) async fn op_config_get(&self, params: &SelfParams) -> Result<Value> {
        let config = self.read_config()?;
        let Some(path) = params.path.as_deref().filter(|p| !p.is_empty()) else {
            // Full view with API keys masked.
            let mut settings = serde_json::to_value(Settings::from(&config))?;
            mask_sensitive_config(&mut settings);
            return Ok(settings);
        };
        let mut root = serde_json::to_value(&config)?;
        mask_sensitive_config(&mut root);
        let value = value_at(&root, path)
            .ok_or_else(|| anyhow::anyhow!("config key '{}' not found", path))?
            .clone();
        Ok(value)
    }

    /// Enable/disable a builtin tool. Persistence and runtime application
    /// stay in one domain module so the config update cannot silently omit the
    /// live catalog change.
    pub(super) async fn op_tool_set(&self, params: &SelfParams, enabled: bool) -> Result<Value> {
        let name = params
            .name
            .as_deref()
            .filter(|n| !n.is_empty())
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "name is required (the builtin tool to {}able)",
                    if enabled { "en" } else { "dis" }
                )
            })?;
        let mut settings = self.config_service()?.snapshot()?.config.tool_settings;
        settings.entry(name.to_string()).or_default().enabled = enabled;
        self.config_service()?
            .apply_patch(ConfigPatch::Tools(settings))?;
        // Runtime apply: in-memory tool_settings + catalog rebuild. Skipped
        // (config still persisted) in headless/test builds without a manager.
        if let Some(tool_control) = &self.context.tool_control {
            tool_control.set_tool_enabled(name, enabled).await?;
        }
        Ok(serde_json::json!({
            "name": name,
            "enabled": enabled,
            "saved": true,
            "note": "take effect immediately"
        }))
    }
}
