//! Shared validation for values that cross the config and JavaScript boundary.

use super::{
    AppConfig, MAX_SESSION_STEPS_PER_RUN, MAX_SESSION_STEPS_PER_SESSION, MIN_SESSION_STEPS_PER_RUN,
    MIN_SESSION_STEPS_PER_SESSION, Settings,
};
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
fn validate_app_config_numbers(config: &AppConfig) -> Result<()> {
    validate_config_numbers(config, &config.llm, &config.context_limits, &config.media)
}

/// Validate the Settings payload before it is applied from IPC.
#[cfg(test)]
fn validate_settings_numbers(settings: &Settings) -> Result<()> {
    validate_config_numbers(
        settings,
        &settings.llm,
        &settings.context_limits,
        &settings.media,
    )
}

/// Validate configuration invariants shared by TOML load/save and typed edits.
/// Keep persistence-only secret checks in their owning boundaries.
pub(super) fn validate_app_config(config: &AppConfig) -> Result<()> {
    validate_app_config_numbers(config)?;
    validate_session_step_budgets(
        config.session.max_steps_per_run,
        config.session.max_steps_per_session,
    )?;
    validate_concurrency_limits(&config.session, &config.llm, &config.context_limits)?;
    validate_resource_limits(&config.session, &config.context_limits)?;
    validate_audio_capture(&config.media.audio)?;
    validate_unique_llm_configuration_keys(&config.llm)?;
    if config.context_limits.max_known_facts > super::MAX_FACTS_IN_EXTRACTION_PROMPT {
        anyhow::bail!(
            "context_limits.max_known_facts must not exceed {}",
            super::MAX_FACTS_IN_EXTRACTION_PROMPT
        );
    }
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

/// Validate the shared step-budget contract used by TOML, Settings and the
/// runtime setters. The UI represents an unlimited lifetime cap as `None`;
/// zero is never a valid configured budget.
pub fn validate_session_step_budgets(
    max_steps_per_run: u32,
    max_steps_per_session: Option<u32>,
) -> Result<()> {
    validate_max_steps_per_run(max_steps_per_run)?;
    validate_max_steps_per_session(max_steps_per_session)
}

pub fn validate_max_steps_per_run(max_steps_per_run: u32) -> Result<()> {
    if !(MIN_SESSION_STEPS_PER_RUN..=MAX_SESSION_STEPS_PER_RUN).contains(&max_steps_per_run) {
        anyhow::bail!(
            "session.max_steps_per_run must be within {MIN_SESSION_STEPS_PER_RUN}..={MAX_SESSION_STEPS_PER_RUN}"
        );
    }
    Ok(())
}

pub fn validate_max_steps_per_session(max_steps_per_session: Option<u32>) -> Result<()> {
    if let Some(max_steps_per_session) = max_steps_per_session
        && !(MIN_SESSION_STEPS_PER_SESSION..=MAX_SESSION_STEPS_PER_SESSION)
            .contains(&max_steps_per_session)
    {
        anyhow::bail!(
            "session.max_steps_per_session must be null or within {MIN_SESSION_STEPS_PER_SESSION}..={MAX_SESSION_STEPS_PER_SESSION}"
        );
    }
    Ok(())
}

pub fn validate_session_concurrency_limit(max_concurrent: usize) -> Result<()> {
    if !(super::MIN_SESSION_MAX_CONCURRENT..=super::MAX_SESSION_MAX_CONCURRENT)
        .contains(&max_concurrent)
    {
        anyhow::bail!(
            "session.max_concurrent must be within {}..={}",
            super::MIN_SESSION_MAX_CONCURRENT,
            super::MAX_SESSION_MAX_CONCURRENT
        );
    }
    Ok(())
}

fn validate_concurrency_limits(
    session: &super::SessionConfig,
    llm: &super::LlmConfig,
    context: &super::ContextLimitsConfig,
) -> Result<()> {
    validate_session_concurrency_limit(session.max_concurrent)?;
    if !(super::MIN_LLM_CONCURRENT_REQUESTS..=super::MAX_LLM_CONCURRENT_REQUESTS)
        .contains(&llm.max_concurrent_requests)
    {
        anyhow::bail!(
            "llm.max_concurrent_requests must be within {}..={}",
            super::MIN_LLM_CONCURRENT_REQUESTS,
            super::MAX_LLM_CONCURRENT_REQUESTS
        );
    }
    if context.max_running_background_tool_runs > super::MAX_RUNNING_BACKGROUND_TOOL_RUNS {
        anyhow::bail!(
            "context_limits.max_running_background_tool_runs must not exceed {}",
            super::MAX_RUNNING_BACKGROUND_TOOL_RUNS
        );
    }
    Ok(())
}

fn validate_resource_limits(
    session: &super::SessionConfig,
    context: &super::ContextLimitsConfig,
) -> Result<()> {
    if session.prompt_history_limit > super::MAX_SESSION_PROMPT_HISTORY_LIMIT {
        anyhow::bail!(
            "session.prompt_history_limit must not exceed {}",
            super::MAX_SESSION_PROMPT_HISTORY_LIMIT
        );
    }
    if context.file_max_list_entries > super::MAX_FILE_MAX_LIST_ENTRIES {
        anyhow::bail!(
            "context_limits.file_max_list_entries must not exceed {}",
            super::MAX_FILE_MAX_LIST_ENTRIES
        );
    }
    if context.mcp_max_sse_buffer_bytes > super::MAX_MCP_MAX_SSE_BUFFER_BYTES {
        anyhow::bail!(
            "context_limits.mcp_max_sse_buffer_bytes must not exceed {} bytes",
            super::MAX_MCP_MAX_SSE_BUFFER_BYTES
        );
    }
    if context.skills_max_md_bytes > super::MAX_SKILLS_MAX_MD_BYTES {
        anyhow::bail!(
            "context_limits.skills_max_md_bytes must not exceed {} bytes",
            super::MAX_SKILLS_MAX_MD_BYTES
        );
    }
    if context.network_max_body_bytes > super::MAX_NETWORK_MAX_BODY_BYTES {
        anyhow::bail!(
            "context_limits.network_max_body_bytes must not exceed {} bytes",
            super::MAX_NETWORK_MAX_BODY_BYTES
        );
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
    for (index, model_config) in config.llm.models.iter().enumerate() {
        if model_config.id.trim().is_empty() {
            anyhow::bail!("llm.models[{index}].id must not be empty");
        }
        if model_config.model.trim().is_empty() {
            anyhow::bail!("llm.models[{index}].model must not be empty");
        }
        if model_config.provider_name.trim().is_empty()
            || !config
                .llm
                .providers
                .iter()
                .any(|provider| provider.name == model_config.provider_name)
        {
            anyhow::bail!("llm.models[{index}].provider_name must reference a configured Provider");
        }
    }
    Ok(())
}

/// Reject duplicate names that are resolved by first-match lookup in the LLM
/// config. This shared check applies equally to TOML loads and Settings edits.
fn validate_unique_llm_configuration_keys(llm: &super::LlmConfig) -> Result<()> {
    let mut provider_names = HashSet::with_capacity(llm.providers.len());
    for (index, provider) in llm.providers.iter().enumerate() {
        if provider.name.trim().is_empty() {
            anyhow::bail!("llm.providers[{index}].name must not be empty");
        }
        if !provider_names.insert(provider.name.as_str()) {
            anyhow::bail!("llm.providers[{index}].name must be unique");
        }
    }

    let mut model_config_ids = HashSet::with_capacity(llm.models.len());
    for (index, model_config) in llm.models.iter().enumerate() {
        if !model_config_ids.insert(model_config.id.as_str()) {
            anyhow::bail!("llm.models[{index}].id must be unique");
        }
    }

    let mut request_kinds = HashSet::with_capacity(llm.request_policies.len());
    for (index, policy) in llm.request_policies.iter().enumerate() {
        if !request_kinds.insert(policy.request) {
            anyhow::bail!("llm.request_policies[{index}].request must be unique");
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
    validate_media_timeouts(media)?;
    if context.scheduled_tool_runs_due_horizon_secs < 0 {
        anyhow::bail!("context_limits.scheduled_tool_runs_due_horizon_secs must be non-negative");
    }
    Ok(())
}

fn validate_media_timeouts(media: &super::MediaConfig) -> Result<()> {
    for (path, value, min, max) in [
        ("media.stt.timeout_secs", media.stt.timeout_secs, 5, 600),
        ("media.ocr.timeout_secs", media.ocr.timeout_secs, 5, 300),
        ("media.tts.timeout_secs", media.tts.timeout_secs, 5, 300),
        (
            "media.image_gen.timeout_secs",
            media.image_gen.timeout_secs,
            10,
            600,
        ),
    ] {
        if !(min..=max).contains(&value) {
            anyhow::bail!("{path} must be within {min}..={max} seconds");
        }
    }
    Ok(())
}

fn validate_audio_capture(audio: &super::AudioConfig) -> Result<()> {
    if !(super::MIN_AUDIO_MAX_DURATION_SECS..=super::MAX_AUDIO_MAX_DURATION_SECS)
        .contains(&audio.max_duration_secs)
    {
        anyhow::bail!(
            "media.audio.max_duration_secs must be within {}..={} seconds",
            super::MIN_AUDIO_MAX_DURATION_SECS,
            super::MAX_AUDIO_MAX_DURATION_SECS
        );
    }
    if !(super::MIN_AUDIO_SILENCE_TIMEOUT_MS..=super::MAX_AUDIO_SILENCE_TIMEOUT_MS)
        .contains(&audio.silence_timeout_ms)
    {
        anyhow::bail!(
            "media.audio.silence_timeout_ms must be within {}..={} milliseconds",
            super::MIN_AUDIO_SILENCE_TIMEOUT_MS,
            super::MAX_AUDIO_SILENCE_TIMEOUT_MS
        );
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
        MAX_INPUT_RING_BUFFER_SECS, MAX_SESSION_STEPS_PER_RUN, MAX_SESSION_STEPS_PER_SESSION,
        MIN_SESSION_STEPS_PER_RUN, MIN_SESSION_STEPS_PER_SESSION, validate_app_config,
        validate_app_config_numbers, validate_settings_contract, validate_settings_numbers,
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
    fn media_timeout_ranges_match_settings_controls_for_config_and_settings() {
        let cases: [(&str, u64, u64, fn(&mut AppConfig, u64)); 4] = [
            ("media.stt.timeout_secs", 5, 600, |config, value| {
                config.media.stt.timeout_secs = value;
            }),
            ("media.ocr.timeout_secs", 5, 300, |config, value| {
                config.media.ocr.timeout_secs = value;
            }),
            ("media.tts.timeout_secs", 5, 300, |config, value| {
                config.media.tts.timeout_secs = value;
            }),
            ("media.image_gen.timeout_secs", 10, 600, |config, value| {
                config.media.image_gen.timeout_secs = value;
            }),
        ];

        for (path, min, max, set_value) in cases {
            for value in [min, max] {
                let mut config = AppConfig::default();
                set_value(&mut config, value);
                assert!(validate_app_config(&config).is_ok(), "{path}={value}");
                assert!(
                    validate_settings_contract(&Settings::from(&config)).is_ok(),
                    "Settings {path}={value}"
                );
            }

            for value in [0, min - 1, max + 1] {
                let mut config = AppConfig::default();
                set_value(&mut config, value);
                let config_error = validate_app_config(&config).unwrap_err();
                assert!(config_error.to_string().contains(path), "{config_error}");
                let settings_error =
                    validate_settings_contract(&Settings::from(&config)).unwrap_err();
                assert!(
                    settings_error.to_string().contains(path),
                    "{settings_error}"
                );
            }
        }
    }

    #[test]
    fn session_step_budget_ranges_match_settings_controls_for_config_and_settings() {
        for (max_steps_per_run, max_steps_per_session) in [
            (MIN_SESSION_STEPS_PER_RUN, None),
            (
                MAX_SESSION_STEPS_PER_RUN,
                Some(MIN_SESSION_STEPS_PER_SESSION),
            ),
            (
                MIN_SESSION_STEPS_PER_RUN,
                Some(MAX_SESSION_STEPS_PER_SESSION),
            ),
        ] {
            let mut config = AppConfig::default();
            config.session.max_steps_per_run = max_steps_per_run;
            config.session.max_steps_per_session = max_steps_per_session;
            assert!(validate_app_config(&config).is_ok());
            assert!(validate_settings_contract(&Settings::from(&config)).is_ok());
        }

        for (max_steps_per_run, max_steps_per_session, expected_path) in [
            (0, None, "max_steps_per_run"),
            (MAX_SESSION_STEPS_PER_RUN + 1, None, "max_steps_per_run"),
            (MIN_SESSION_STEPS_PER_RUN, Some(0), "max_steps_per_session"),
            (
                MIN_SESSION_STEPS_PER_RUN,
                Some(MAX_SESSION_STEPS_PER_SESSION + 1),
                "max_steps_per_session",
            ),
        ] {
            let mut config = AppConfig::default();
            config.session.max_steps_per_run = max_steps_per_run;
            config.session.max_steps_per_session = max_steps_per_session;
            let config_error = validate_app_config(&config).unwrap_err();
            assert!(config_error.to_string().contains(expected_path));
            let settings_error = validate_settings_contract(&Settings::from(&config)).unwrap_err();
            assert!(settings_error.to_string().contains(expected_path));
        }
    }

    #[test]
    fn audio_capture_ranges_match_settings_controls_for_config_and_settings() {
        for (duration, silence) in [
            (
                super::super::MIN_AUDIO_MAX_DURATION_SECS,
                super::super::MIN_AUDIO_SILENCE_TIMEOUT_MS,
            ),
            (
                super::super::MAX_AUDIO_MAX_DURATION_SECS,
                super::super::MAX_AUDIO_SILENCE_TIMEOUT_MS,
            ),
        ] {
            let mut config = AppConfig::default();
            config.media.audio.max_duration_secs = duration;
            config.media.audio.silence_timeout_ms = silence;
            assert!(validate_app_config(&config).is_ok());
            assert!(validate_settings_contract(&Settings::from(&config)).is_ok());
        }

        for (duration, silence, expected_path) in [
            (0, 1_500, "max_duration_secs"),
            (301, 1_500, "max_duration_secs"),
            (60, 499, "silence_timeout_ms"),
            (60, 10_001, "silence_timeout_ms"),
        ] {
            let mut config = AppConfig::default();
            config.media.audio.max_duration_secs = duration;
            config.media.audio.silence_timeout_ms = silence;
            assert!(
                validate_app_config(&config)
                    .unwrap_err()
                    .to_string()
                    .contains(expected_path)
            );
            assert!(
                validate_settings_contract(&Settings::from(&config))
                    .unwrap_err()
                    .to_string()
                    .contains(expected_path)
            );
        }
    }

    #[test]
    fn concurrency_limits_match_config_settings_and_runtime_ranges() {
        for (session, llm, background) in [
            (
                super::super::MIN_SESSION_MAX_CONCURRENT,
                super::super::MIN_LLM_CONCURRENT_REQUESTS,
                0,
            ),
            (
                super::super::MAX_SESSION_MAX_CONCURRENT,
                super::super::MAX_LLM_CONCURRENT_REQUESTS,
                super::super::MAX_RUNNING_BACKGROUND_TOOL_RUNS,
            ),
        ] {
            let mut config = AppConfig::default();
            config.session.max_concurrent = session;
            config.llm.max_concurrent_requests = llm;
            config.context_limits.max_running_background_tool_runs = background;
            assert!(validate_app_config(&config).is_ok());
            assert!(validate_settings_contract(&Settings::from(&config)).is_ok());
        }

        for (session, llm, background, expected_path) in [
            (0, 2, 64, "session.max_concurrent"),
            (11, 2, 64, "session.max_concurrent"),
            (3, 0, 64, "llm.max_concurrent_requests"),
            (3, 17, 64, "llm.max_concurrent_requests"),
            (3, 2, 65, "context_limits.max_running_background_tool_runs"),
        ] {
            let mut config = AppConfig::default();
            config.session.max_concurrent = session;
            config.llm.max_concurrent_requests = llm;
            config.context_limits.max_running_background_tool_runs = background;
            assert!(
                validate_app_config(&config)
                    .unwrap_err()
                    .to_string()
                    .contains(expected_path)
            );
            assert!(
                validate_settings_contract(&Settings::from(&config))
                    .unwrap_err()
                    .to_string()
                    .contains(expected_path)
            );
        }
    }

    #[test]
    fn resource_bounds_match_config_and_settings_contracts() {
        let cases: [(&str, u64, u64, fn(&mut AppConfig, u64)); 5] = [
            (
                "session.prompt_history_limit",
                0,
                super::super::MAX_SESSION_PROMPT_HISTORY_LIMIT as u64,
                |config, value| config.session.prompt_history_limit = value as usize,
            ),
            (
                "context_limits.file_max_list_entries",
                0,
                super::super::MAX_FILE_MAX_LIST_ENTRIES as u64,
                |config, value| config.context_limits.file_max_list_entries = value as usize,
            ),
            (
                "context_limits.mcp_max_sse_buffer_bytes",
                0,
                super::super::MAX_MCP_MAX_SSE_BUFFER_BYTES as u64,
                |config, value| config.context_limits.mcp_max_sse_buffer_bytes = value as usize,
            ),
            (
                "context_limits.skills_max_md_bytes",
                0,
                super::super::MAX_SKILLS_MAX_MD_BYTES,
                |config, value| config.context_limits.skills_max_md_bytes = value,
            ),
            (
                "context_limits.network_max_body_bytes",
                0,
                super::super::MAX_NETWORK_MAX_BODY_BYTES as u64,
                |config, value| config.context_limits.network_max_body_bytes = value as usize,
            ),
        ];

        for (path, min, max, set_value) in cases {
            for value in [min, max] {
                let mut config = AppConfig::default();
                set_value(&mut config, value);
                assert!(validate_app_config(&config).is_ok(), "{path}={value}");
                assert!(
                    validate_settings_contract(&Settings::from(&config)).is_ok(),
                    "Settings {path}={value}"
                );
            }

            for value in min.checked_sub(1).into_iter().chain([max + 1]) {
                let mut config = AppConfig::default();
                set_value(&mut config, value);
                assert!(
                    validate_app_config(&config)
                        .unwrap_err()
                        .to_string()
                        .contains(path),
                    "config {path}={value}"
                );
                assert!(
                    validate_settings_contract(&Settings::from(&config))
                        .unwrap_err()
                        .to_string()
                        .contains(path),
                    "Settings {path}={value}"
                );
            }
        }
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
    fn caps_known_facts_in_extraction_prompt_for_config_and_settings() {
        let mut config = AppConfig::default();
        config.context_limits.max_known_facts = super::super::MAX_FACTS_IN_EXTRACTION_PROMPT + 1;

        let config_error = validate_app_config(&config).unwrap_err();
        assert!(config_error.to_string().contains("max_known_facts"));
        let settings_error = validate_settings_contract(&Settings::from(&config)).unwrap_err();
        assert!(settings_error.to_string().contains("max_known_facts"));
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
    fn config_and_settings_reject_ambiguous_llm_collection_keys() {
        let mut config = AppConfig::default();
        config.llm.providers.extend([
            ProviderConfig {
                name: "primary".into(),
                ..Default::default()
            },
            ProviderConfig {
                name: "primary".into(),
                ..Default::default()
            },
        ]);
        for validate in [
            validate_app_config(&config),
            validate_settings_contract(&Settings::from(&config)),
        ] {
            assert!(
                validate
                    .unwrap_err()
                    .to_string()
                    .contains("name must be unique")
            );
        }

        let mut config = AppConfig::default();
        config.llm.models.extend([
            crate::config::ModelConfig {
                id: "chat-slot".into(),
                ..Default::default()
            },
            crate::config::ModelConfig {
                id: "chat-slot".into(),
                ..Default::default()
            },
        ]);
        for validate in [
            validate_app_config(&config),
            validate_settings_contract(&Settings::from(&config)),
        ] {
            assert!(
                validate
                    .unwrap_err()
                    .to_string()
                    .contains("id must be unique")
            );
        }

        let mut config = AppConfig::default();
        config.llm.request_policies.extend([
            crate::config::RequestPolicy {
                request: crate::config::RequestKind::Chat,
                primary: "first".into(),
            },
            crate::config::RequestPolicy {
                request: crate::config::RequestKind::Chat,
                primary: "second".into(),
            },
        ]);
        for validate in [
            validate_app_config(&config),
            validate_settings_contract(&Settings::from(&config)),
        ] {
            assert!(
                validate
                    .unwrap_err()
                    .to_string()
                    .contains("request must be unique")
            );
        }
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
