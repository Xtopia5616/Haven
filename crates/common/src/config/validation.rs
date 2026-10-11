//! Shared validation for values that cross the config and JavaScript boundary.

use super::{AppConfig, Settings};
use anyhow::{Context, Result};
use serde::Serialize;
use serde_json::Value;

const JS_MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;

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
    use super::{validate_app_config_numbers, validate_settings_numbers};
    use crate::config::{AppConfig, Settings};

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
}
