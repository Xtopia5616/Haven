//! Shared validation for values that cross the config and JavaScript boundary.

use super::{AppConfig, Settings};
use anyhow::{Context, Result};
use serde::Serialize;
use serde_json::Value;
use std::collections::HashSet;

const JS_MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;
/// The capture ring stores mono 16 kHz `f32` samples; this cap bounds its
/// allocation to about 7.7 MB while allowing two minutes of audio history.
pub const MAX_INPUT_RING_BUFFER_SECS: usize = 120;

/// Validate persisted config numbers before they are loaded into runtime state
/// or written to disk. Integer fields are also exposed as JavaScript `number`
/// values through Settings, so they must remain exactly representable there.
pub fn validate_app_config_numbers(config: &AppConfig) -> Result<()> {
    validate_config_numbers(config, &config.llm, &config.context_limits, &config.media)
}

/// Validate the Settings payload before it is applied from IPC.
pub fn validate_settings_numbers(settings: &Settings) -> Result<()> {
    validate_config_numbers(
        settings,
        &settings.llm,
        &settings.context_limits,
        &settings.media,
    )
}

/// Validate configuration invariants shared by TOML load/save and typed edits.
/// Keep persistence-only secret checks in their owning boundaries.
pub fn validate_app_config(config: &AppConfig) -> Result<()> {
    validate_app_config_numbers(config)?;
    super::validate_mcp_server_names(&config.mcp_servers)?;
    validate_mcp_discovery(&config.mcp_discovery)?;
    if config.context_limits.input_ring_buffer_secs > MAX_INPUT_RING_BUFFER_SECS {
        anyhow::bail!(
            "context_limits.input_ring_buffer_secs must not exceed {MAX_INPUT_RING_BUFFER_SECS}"
        );
    }

    for provider in &config.llm.providers {
        if provider
            .api_style
            .as_deref()
            .is_some_and(|style| !style.trim().is_empty() && !super::is_known_api_style(style))
        {
            anyhow::bail!("llm.providers.api_style must use a supported API style");
        }
    }

    let has_provider = |name: &str| {
        config
            .llm
            .providers
            .iter()
            .any(|provider| provider.name == name)
    };
    if !matches!(
        config.media.stt.provider_name.as_str(),
        "llm" | "mcp" | "none"
    ) && !has_provider(&config.media.stt.provider_name)
    {
        anyhow::bail!("media.stt.provider_name must reference a configured Provider");
    }
    if config.media.tts.provider_name != "none" && !has_provider(&config.media.tts.provider_name) {
        anyhow::bail!("media.tts.provider_name must reference a configured Provider or none");
    }
    if config.media.image_gen.provider_name != "none"
        && !has_provider(&config.media.image_gen.provider_name)
    {
        anyhow::bail!("media.image_gen.provider_name must reference a configured Provider or none");
    }
    Ok(())
}

/// Validate Settings before accepting it from Tauri IPC. This uses the same
/// semantic rules as config persistence and rejects values that must be
/// staged through the secure credential commands.
pub fn validate_settings_contract(settings: &Settings) -> Result<()> {
    let config = AppConfig::from(settings);
    validate_app_config(&config)?;
    super::credentials::validate_reference_syntax(&config)?;
    if config
        .llm
        .providers
        .iter()
        .any(|provider| !provider.api_key.is_empty())
        || !config.media.ocr.api_key.is_empty()
        || !config.media.ocr.api_secret.is_empty()
    {
        anyhow::bail!(
            "credential values must be staged in secure storage before Settings can be saved"
        );
    }
    if config
        .mcp_servers
        .iter()
        .any(|server| !server.env.is_empty())
    {
        anyhow::bail!("MCP environment values must be changed through the MCP settings commands");
    }
    let mut model_ids = HashSet::with_capacity(config.llm.models.len());
    for (index, model) in config.llm.models.iter().enumerate() {
        if model.id.trim().is_empty() {
            anyhow::bail!("llm.models[{index}].id must not be empty");
        }
        if !model_ids.insert(model.id.as_str()) {
            anyhow::bail!("llm.models[{index}].id must be unique");
        }
        if model.model.trim().is_empty() {
            anyhow::bail!("llm.models[{index}].model must not be empty");
        }
        if model.provider_name.trim().is_empty()
            || !config
                .llm
                .providers
                .iter()
                .any(|provider| provider.name == model.provider_name)
        {
            anyhow::bail!("llm.models[{index}].provider_name must reference a configured Provider");
        }
    }
    Ok(())
}

fn validate_config_numbers(
    value: &impl Serialize,
    llm: &super::LlmConfig,
    context: &super::ContextLimitsConfig,
    media: &super::MediaConfig,
) -> Result<()> {
    validate_numeric_contract(value)?;
    validate_float_fields(llm, context, media)?;
    if context.scheduled_tool_runs_due_horizon_secs < 0 {
        anyhow::bail!("context_limits.scheduled_tool_runs_due_horizon_secs must be non-negative");
    }
    Ok(())
}

fn validate_mcp_discovery(config: &super::McpDiscoveryConfig) -> Result<()> {
    if config.health_interval_secs == 0 {
        anyhow::bail!("mcp_discovery.health_interval_secs must be greater than zero");
    }
    if config.reconnect_initial_ms == 0 {
        anyhow::bail!("mcp_discovery.reconnect_initial_ms must be greater than zero");
    }
    if config.reconnect_max_ms == 0 || config.reconnect_max_ms < config.reconnect_initial_ms {
        anyhow::bail!(
            "mcp_discovery.reconnect_max_ms must be at least reconnect_initial_ms and greater than zero"
        );
    }
    Ok(())
}

fn validate_numeric_contract(value: &impl Serialize) -> Result<()> {
    let value = serde_json::to_value(value).context("serialize config for numeric validation")?;
    validate_json_numbers(&value, "config")
}

fn validate_json_numbers(value: &Value, path: &str) -> Result<()> {
    match value {
        Value::Number(number) => {
            if number
                .as_u64()
                .is_some_and(|value| value > JS_MAX_SAFE_INTEGER)
                || number
                    .as_i64()
                    .is_some_and(|value| value.unsigned_abs() > JS_MAX_SAFE_INTEGER)
            {
                anyhow::bail!("{path} exceeds the JavaScript safe integer range");
            }
            if number.as_f64().is_some_and(|value| !value.is_finite()) {
                anyhow::bail!("{path} must be finite");
            }
        }
        Value::Array(values) => {
            for (index, value) in values.iter().enumerate() {
                validate_json_numbers(value, &format!("{path}[{index}]"))?;
            }
        }
        Value::Object(values) => {
            for (key, value) in values {
                validate_json_numbers(value, &format!("{path}.{key}"))?;
            }
        }
        Value::Null | Value::Bool(_) | Value::String(_) => {}
    }
    Ok(())
}

fn validate_float_fields(
    llm: &super::LlmConfig,
    context: &super::ContextLimitsConfig,
    media: &super::MediaConfig,
) -> Result<()> {
    validate_range("llm.retry_jitter", f64::from(llm.retry_jitter), 0.0, 1.0)?;
    validate_range(
        "context_limits.compaction_ratio",
        f64::from(context.compaction_ratio),
        0.1,
        0.95,
    )?;
    validate_range(
        "context_limits.attachment_image_jpeg_quality",
        f64::from(context.attachment_image_jpeg_quality),
        0.0,
        1.0,
    )?;
    validate_range(
        "media.audio.vad_threshold",
        f64::from(media.audio.vad_threshold),
        0.0,
        1.0,
    )?;
    validate_range(
        "media.stt.min_confidence",
        f64::from(media.stt.min_confidence),
        0.0,
        1.0,
    )?;
    validate_range(
        "media.ocr.min_confidence",
        f64::from(media.ocr.min_confidence),
        0.0,
        1.0,
    )?;

    for (index, provider) in llm.providers.iter().enumerate() {
        if let Some(temperature) = provider.default_temperature {
            validate_range(
                &format!("llm.providers[{index}].default_temperature"),
                f64::from(temperature),
                0.0,
                2.0,
            )?;
        }
    }
    for (index, model) in llm.models.iter().enumerate() {
        if let Some(temperature) = model.temperature {
            validate_range(
                &format!("llm.models[{index}].temperature"),
                f64::from(temperature),
                0.0,
                2.0,
            )?;
        }
        for (field, value) in [
            ("cost_per_1k_input_tokens", model.cost_per_1k_input_tokens),
            ("cost_per_1k_output_tokens", model.cost_per_1k_output_tokens),
            (
                "cost_per_1k_cache_read_tokens",
                model.cost_per_1k_cache_read_tokens,
            ),
            (
                "cost_per_1k_cache_write_tokens",
                model.cost_per_1k_cache_write_tokens,
            ),
        ] {
            if let Some(value) = value {
                validate_nonnegative_finite(&format!("llm.models[{index}].{field}"), value)?;
            }
        }
    }
    Ok(())
}

fn validate_nonnegative_finite(path: &str, value: f64) -> Result<()> {
    if !value.is_finite() || value < 0.0 {
        anyhow::bail!("{path} must be finite and non-negative");
    }
    Ok(())
}

fn validate_range(path: &str, value: f64, min: f64, max: f64) -> Result<()> {
    if !value.is_finite() || !(min..=max).contains(&value) {
        anyhow::bail!("{path} must be finite and within {min}..={max}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        MAX_INPUT_RING_BUFFER_SECS, validate_app_config, validate_app_config_numbers,
        validate_settings_contract, validate_settings_numbers,
    };
    use crate::config::{AppConfig, ProviderConfig, Settings};

    #[test]
    fn accepts_defaults_and_safe_integer_boundaries() {
        let mut config = AppConfig::default();
        config.context_limits.max_upload_total_bytes = 9_007_199_254_740_991;
        config.context_limits.default_context_window = u32::MAX;

        assert!(validate_app_config_numbers(&config).is_ok());
        assert!(validate_settings_numbers(&Settings::from(&config)).is_ok());
    }

    #[test]
    fn rejects_integers_that_javascript_cannot_roundtrip_exactly() {
        let mut config = AppConfig::default();
        config.context_limits.max_upload_total_bytes = 9_007_199_254_740_992;

        let error = validate_app_config_numbers(&config).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("context_limits.max_upload_total_bytes")
        );
    }

    #[test]
    fn rejects_non_finite_and_out_of_range_float_settings() {
        let mut config = AppConfig::default();
        config.llm.retry_jitter = f32::NAN;
        assert!(validate_app_config_numbers(&config).is_err());

        config.llm.retry_jitter = 0.2;
        config.context_limits.compaction_ratio = 1.0;
        assert!(validate_app_config_numbers(&config).is_err());

        config.context_limits.compaction_ratio = 0.65;
        config.llm.models.push(crate::config::ModelConfig {
            temperature: Some(2.1),
            ..Default::default()
        });
        assert!(validate_app_config_numbers(&config).is_err());
    }

    #[test]
    fn rejects_negative_scheduled_horizon() {
        let mut settings = Settings::default();
        settings.context_limits.scheduled_tool_runs_due_horizon_secs = -1;
        assert!(validate_settings_numbers(&settings).is_err());
    }

    #[test]
    fn rejects_unbounded_capture_buffer_and_zero_mcp_intervals() {
        let mut config = AppConfig::default();
        config.context_limits.input_ring_buffer_secs = MAX_INPUT_RING_BUFFER_SECS + 1;
        assert!(validate_app_config(&config).is_err());

        config.context_limits.input_ring_buffer_secs = MAX_INPUT_RING_BUFFER_SECS;
        config.mcp_discovery.health_interval_secs = 0;
        assert!(validate_app_config(&config).is_err());

        config.mcp_discovery.health_interval_secs = 15;
        config.mcp_discovery.reconnect_initial_ms = 2_000;
        config.mcp_discovery.reconnect_max_ms = 1_000;
        assert!(validate_app_config(&config).is_err());
    }

    #[test]
    fn app_config_and_settings_share_provider_contract_validation() {
        let mut config = AppConfig::default();
        config.llm.providers.push(ProviderConfig {
            name: "primary".into(),
            api_style: Some("not-a-wire-style".into()),
            ..Default::default()
        });
        assert!(validate_app_config(&config).is_err());
        assert!(validate_settings_contract(&Settings::from(&config)).is_err());

        config.llm.providers[0].api_style = None;
        config.media.tts.provider_name = "missing".into();
        assert!(validate_app_config(&config).is_err());
        assert!(validate_settings_contract(&Settings::from(&config)).is_err());
    }

    #[test]
    fn settings_reject_models_that_would_be_dropped_or_ambiguously_routed() {
        let mut config = AppConfig::default();
        config.llm.providers.push(ProviderConfig {
            name: "primary".into(),
            ..Default::default()
        });
        config.llm.models.push(crate::config::ModelConfig {
            id: "chat-slot".into(),
            provider_name: "primary".into(),
            model: "gpt-test".into(),
            ..Default::default()
        });

        let mut settings = Settings::from(&config);
        settings.llm.models[0].id = "  ".into();
        assert!(
            validate_settings_contract(&settings)
                .unwrap_err()
                .to_string()
                .contains("llm.models[0].id")
        );

        let mut settings = Settings::from(&config);
        settings.llm.models[0].model = "  ".into();
        assert!(
            validate_settings_contract(&settings)
                .unwrap_err()
                .to_string()
                .contains("llm.models[0].model")
        );

        let mut settings = Settings::from(&config);
        settings.llm.models.push(settings.llm.models[0].clone());
        assert!(
            validate_settings_contract(&settings)
                .unwrap_err()
                .to_string()
                .contains("llm.models[1].id must be unique")
        );
    }
}
