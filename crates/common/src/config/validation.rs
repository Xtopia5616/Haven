//! Shared validation for values that cross the config and JavaScript boundary.

use super::{
    AppConfig, MAX_SESSION_STEPS_PER_RUN, MAX_SESSION_STEPS_PER_SESSION, MIN_SESSION_STEPS_PER_RUN,
    MIN_SESSION_STEPS_PER_SESSION, SettingsSnapshot,
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
/// values through SettingsSnapshot, so they must remain exactly representable there.
fn validate_app_config_numbers(config: &AppConfig) -> Result<()> {
    validate_config_numbers(config, &config.llm, &config.context_limits, &config.media)
}

/// Validate the SettingsSnapshot payload before it is applied from IPC.
#[cfg(test)]
fn validate_settings_numbers(settings: &SettingsSnapshot) -> Result<()> {
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
    if config
        .security
        .execution_root
        .as_ref()
        .is_some_and(|root| !crate::path::is_absolute_local_path(root))
    {
        anyhow::bail!("security.execution_root must be an absolute local directory path");
    }
    validate_session_step_budgets(
        config.session.max_steps_per_run,
        config.session.max_steps_per_session,
    )?;
    validate_concurrency_limits(&config.session, &config.llm, &config.context_limits)?;
    validate_resource_limits(&config.session, &config.context_limits)?;
    validate_skills_exec(&config.skills_exec)?;
    validate_context_windows(config)?;
    for (name, tool) in &config.tool_settings {
        if tool
            .timeout_secs
            .is_some_and(|timeout| timeout == 0 || timeout > super::MAX_TOOL_TIMEOUT_SECS)
        {
            anyhow::bail!(
                "tool_settings.{name}.timeout_secs must be within 1..={} seconds",
                super::MAX_TOOL_TIMEOUT_SECS
            );
        }
        if tool
            .max_retries
            .is_some_and(|retries| retries > super::MAX_TOOL_RETRIES)
        {
            anyhow::bail!(
                "tool_settings.{name}.max_retries must not exceed {}",
                super::MAX_TOOL_RETRIES
            );
        }
        if tool
            .retry_backoff_secs
            .is_some_and(|secs| secs > super::MAX_TOOL_RETRY_BACKOFF_SECS)
        {
            anyhow::bail!(
                "tool_settings.{name}.retry_backoff_secs must not exceed {} seconds",
                super::MAX_TOOL_RETRY_BACKOFF_SECS
            );
        }
        if tool
            .max_output_chars
            .is_some_and(|cap| cap > config.context_limits.max_observation_chars)
        {
            anyhow::bail!(
                "tool_settings.{name}.max_output_chars must not exceed context_limits.max_observation_chars"
            );
        }
    }
    validate_audio_capture(&config.media.audio)?;
    validate_ocr_provider(&config.media.ocr.provider)?;
    validate_unique_llm_configuration_keys(&config.llm)?;
    if config.context_limits.max_known_facts > super::MAX_FACTS_IN_EXTRACTION_PROMPT {
        anyhow::bail!(
            "context_limits.max_known_facts must not exceed {}",
            super::MAX_FACTS_IN_EXTRACTION_PROMPT
        );
    }
    super::validate_mcp_server_names(&config.mcp_servers)?;
    super::validate_mcp_server_exposure(&config.mcp_servers)?;
    super::validate_mcp_server_execution(&config.mcp_servers)?;
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
        let supported = super::media_capabilities_supported_by_style(
            provider
                .api_style
                .as_deref()
                .filter(|style| !style.trim().is_empty())
                .unwrap_or("openai-chat"),
        );
        let capabilities = provider.media_capabilities;
        if (capabilities.stt && !supported.stt)
            || (capabilities.tts && !supported.tts)
            || (capabilities.image_gen && !supported.image_gen)
        {
            anyhow::bail!(
                "llm.providers.media_capabilities cannot enable endpoints unsupported by api_style"
            );
        }
        validate_web_search_mode(
            "llm.providers.default_web_search",
            provider.default_web_search.as_deref(),
        )?;
    }
    for (index, model) in config.llm.models.iter().enumerate() {
        validate_web_search_mode(
            &format!("llm.models[{index}].web_search"),
            model.web_search.as_deref(),
        )?;
        if let Some(search) = &model.independent_search {
            let provider = config
                .llm
                .provider_config_by_name(&model.provider_name)
                .map(|provider| provider.provider.trim().to_ascii_lowercase());
            if provider.as_deref() != Some(search.provider_id()) {
                anyhow::bail!(
                    "llm.models[{index}].independent_search must match the selected Provider"
                );
            }
            match search {
                super::IndependentSearchConfig::DeepseekSearch { count, .. }
                    if !(1..=50).contains(count) =>
                {
                    anyhow::bail!(
                        "llm.models[{index}].independent_search.count must be within 1..=50"
                    );
                }
                super::IndependentSearchConfig::DeepseekSearch { localization, .. }
                    if localization
                        .as_ref()
                        .is_some_and(|ip| ip.parse::<std::net::IpAddr>().is_err()) =>
                {
                    anyhow::bail!(
                        "llm.models[{index}].independent_search.localization must be a valid IP address"
                    );
                }
                super::IndependentSearchConfig::PerplexitySearch {
                    max_results,
                    search_type,
                    country,
                    max_tokens,
                    max_tokens_per_page,
                    search_language_filter,
                    search_domain_filter,
                    last_updated_after_filter,
                    last_updated_before_filter,
                    search_after_date_filter,
                    search_before_date_filter,
                    ..
                } => {
                    if *max_results == 0
                        || match search_type {
                            super::PerplexitySearchType::People => *max_results > 50,
                            super::PerplexitySearchType::Web
                            | super::PerplexitySearchType::Fast => *max_results > 20,
                        }
                    {
                        anyhow::bail!(
                            "llm.models[{index}].independent_search.max_results is outside the selected Perplexity search_type limit"
                        );
                    }
                    if country.as_ref().is_some_and(|country| {
                        country.len() != 2
                            || !country.bytes().all(|byte| byte.is_ascii_alphabetic())
                    }) {
                        anyhow::bail!(
                            "llm.models[{index}].independent_search.country must be a two-letter country code"
                        );
                    }
                    if max_tokens.is_some_and(|tokens| tokens == 0 || tokens > 1_000_000)
                        || max_tokens_per_page
                            .is_some_and(|tokens| tokens == 0 || tokens > 1_000_000)
                    {
                        anyhow::bail!(
                            "llm.models[{index}].independent_search token limits must be within 1..=1000000"
                        );
                    }
                    if search_language_filter.len() > 20
                        || search_language_filter.iter().any(|language| {
                            language.len() != 2
                                || !language.bytes().all(|byte| byte.is_ascii_alphabetic())
                        })
                    {
                        anyhow::bail!(
                            "llm.models[{index}].independent_search.search_language_filter must contain at most 20 two-letter language codes"
                        );
                    }
                    if search_domain_filter.len() > 20
                        || search_domain_filter.iter().any(|domain| {
                            domain.is_empty()
                                || domain.len() > 253
                                || domain.chars().any(char::is_whitespace)
                        })
                    {
                        anyhow::bail!(
                            "llm.models[{index}].independent_search.search_domain_filter must contain at most 20 domains of at most 253 characters"
                        );
                    }
                    for (field, date) in [
                        ("last_updated_after_filter", last_updated_after_filter),
                        ("last_updated_before_filter", last_updated_before_filter),
                        ("search_after_date_filter", search_after_date_filter),
                        ("search_before_date_filter", search_before_date_filter),
                    ] {
                        if date.as_deref().is_some_and(|date| !is_valid_mmddyyyy(date)) {
                            anyhow::bail!(
                                "llm.models[{index}].independent_search.{field} must use MM/DD/YYYY"
                            );
                        }
                    }
                }
                super::IndependentSearchConfig::AliyunAiSearch {
                    endpoint,
                    workspace_name,
                    top_k,
                    content_type,
                    way,
                    api_key_ref,
                    ..
                } => {
                    let parsed = url::Url::parse(endpoint.trim()).ok();
                    if !parsed.as_ref().is_some_and(|url| {
                        url.scheme() == "https"
                            && url.host_str().is_some()
                            && url.username().is_empty()
                            && url.password().is_none()
                            && url.query().is_none()
                            && url.fragment().is_none()
                    }) {
                        anyhow::bail!(
                            "llm.models[{index}].independent_search.endpoint must be HTTPS without credentials, query, or fragment"
                        );
                    }
                    if workspace_name.trim().is_empty()
                        || workspace_name.len() > 128
                        || !workspace_name
                            .bytes()
                            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
                    {
                        anyhow::bail!(
                            "llm.models[{index}].independent_search.workspace_name is invalid"
                        );
                    }
                    if !(1..=50).contains(top_k) {
                        anyhow::bail!(
                            "llm.models[{index}].independent_search.top_k must be within 1..=50"
                        );
                    }
                    if *way == super::AliyunSearchWay::ProFetch
                        && *content_type != super::AliyunSearchContentType::MainText
                    {
                        anyhow::bail!(
                            "llm.models[{index}].independent_search.way=pro-fetch requires content_type=mainText"
                        );
                    }
                    if api_key_ref.is_none() {
                        anyhow::bail!(
                            "llm.models[{index}].independent_search requires a separately stored AI Search API key"
                        );
                    }
                }
                _ => {}
            }
        }
    }

    let has_provider = |name: &str| {
        config
            .llm
            .providers
            .iter()
            .any(|provider| provider.name == name.trim())
    };
    let stt_provider = config.media.stt.provider_name.trim();
    if stt_provider.is_empty() {
        anyhow::bail!("media.stt.provider_name must not be empty");
    }
    let stt_mode = if stt_provider.eq_ignore_ascii_case("llm") {
        "llm"
    } else if stt_provider.eq_ignore_ascii_case("mcp") {
        "mcp"
    } else if stt_provider.eq_ignore_ascii_case("none") {
        "none"
    } else {
        "provider"
    };
    if stt_mode == "provider" && !has_provider(stt_provider) {
        anyhow::bail!("media.stt.provider_name must reference a configured Provider");
    }
    if stt_mode == "mcp" {
        let server_name = config
            .media
            .stt
            .mcp_server
            .as_deref()
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .ok_or_else(|| {
                anyhow::anyhow!("media.stt.mcp_server is required when provider_name is mcp")
            })?;
        if !config
            .mcp_servers
            .iter()
            .any(|server| server.name == server_name)
        {
            anyhow::bail!("media.stt.mcp_server must reference a configured MCP server");
        }
    }
    validate_optional_media_provider(
        "media.tts.provider_name",
        &config.media.tts.provider_name,
        &has_provider,
    )?;
    validate_optional_media_provider(
        "media.image_gen.provider_name",
        &config.media.image_gen.provider_name,
        &has_provider,
    )?;
    Ok(())
}

fn is_valid_mmddyyyy(value: &str) -> bool {
    let mut parts = value.split('/');
    let (Some(month), Some(day), Some(year), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return false;
    };
    let (Ok(month), Ok(day), Ok(year)) = (
        month.parse::<u32>(),
        day.parse::<u32>(),
        year.parse::<u32>(),
    ) else {
        return false;
    };
    if !(1..=12).contains(&month) || year == 0 {
        return false;
    }
    let max_day = match month {
        2 if year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    };
    (1..=max_day).contains(&day)
}

fn validate_optional_media_provider(
    path: &str,
    name: &str,
    has_provider: &impl Fn(&str) -> bool,
) -> Result<()> {
    let name = name.trim();
    if name.is_empty() || name.eq_ignore_ascii_case("none") || has_provider(name) {
        Ok(())
    } else {
        anyhow::bail!("{path} must reference a configured Provider or be none/empty")
    }
}

fn validate_ocr_provider(provider: &str) -> Result<()> {
    if matches!(provider, "llm" | "baidu" | "azure" | "tencent" | "none") {
        Ok(())
    } else {
        anyhow::bail!("media.ocr.provider must be one of llm, baidu, azure, tencent, or none")
    }
}

fn validate_web_search_mode(path: &str, value: Option<&str>) -> Result<()> {
    let Some(value) = value else {
        return Ok(());
    };
    let mode = value.trim();
    if mode.is_empty()
        || matches!(
            mode.to_ascii_lowercase().as_str(),
            "off" | "auto" | "always" | "required" | "on" | "1" | "true"
        )
    {
        Ok(())
    } else {
        anyhow::bail!("{path} must be off, auto, always, or a supported always alias")
    }
}

/// Validate the shared step-budget contract used by TOML, SettingsSnapshot and the
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
    if session.history_retention_days > super::MAX_HISTORY_RETENTION_DAYS {
        anyhow::bail!(
            "session.history_retention_days must not exceed {} days",
            super::MAX_HISTORY_RETENTION_DAYS
        );
    }
    if context.file_max_list_entries > super::MAX_FILE_MAX_LIST_ENTRIES {
        anyhow::bail!(
            "context_limits.file_max_list_entries must not exceed {}",
            super::MAX_FILE_MAX_LIST_ENTRIES
        );
    }
    if !(super::MIN_FILE_LINE_MAX_BYTES..=super::MAX_FILE_LINE_MAX_BYTES)
        .contains(&context.file_line_max_bytes)
    {
        anyhow::bail!(
            "context_limits.file_line_max_bytes must be between {} and {} bytes",
            super::MIN_FILE_LINE_MAX_BYTES,
            super::MAX_FILE_LINE_MAX_BYTES
        );
    }
    if !(super::MIN_FILE_BYTE_READ_MAX_BYTES..=super::MAX_FILE_BYTE_READ_MAX_BYTES)
        .contains(&context.file_byte_read_max_bytes)
    {
        anyhow::bail!(
            "context_limits.file_byte_read_max_bytes must be between {} and {} bytes",
            super::MIN_FILE_BYTE_READ_MAX_BYTES,
            super::MAX_FILE_BYTE_READ_MAX_BYTES
        );
    }
    for (path, value, max) in [
        (
            "context_limits.file_max_full_file_bytes",
            context.file_max_full_file_bytes,
            super::MAX_FILE_OPERATION_BYTES,
        ),
        (
            "context_limits.file_write_max_bytes",
            context.file_write_max_bytes,
            super::MAX_FILE_OPERATION_BYTES,
        ),
        (
            "context_limits.file_vision_max_bytes",
            context.file_vision_max_bytes,
            super::MAX_FILE_OPERATION_BYTES,
        ),
        (
            "context_limits.search_max_file_size_bytes",
            context.search_max_file_size_bytes,
            super::MAX_SEARCH_FILE_SIZE_BYTES,
        ),
    ] {
        if value > max {
            anyhow::bail!("{path} must not exceed {max} bytes");
        }
    }
    if context.file_write_max_bytes == 0 {
        anyhow::bail!("context_limits.file_write_max_bytes must be greater than zero");
    }
    for (path, value, max) in [
        (
            "context_limits.max_observation_chars",
            context.max_observation_chars,
            super::MAX_CONTEXT_TEXT_CHARS,
        ),
        (
            "context_limits.max_transcript_chars",
            context.max_transcript_chars,
            super::MAX_CONTEXT_TEXT_CHARS,
        ),
        (
            "context_limits.tool_run_result_context_chars",
            context.tool_run_result_context_chars,
            super::MAX_CONTEXT_TEXT_CHARS,
        ),
        (
            "context_limits.notification_summary_chars",
            context.notification_summary_chars,
            super::MAX_NOTIFICATION_SUMMARY_CHARS,
        ),
        (
            "context_limits.file_summary_input_chars",
            context.file_summary_input_chars,
            super::MAX_FILE_SUMMARY_INPUT_CHARS,
        ),
        (
            "context_limits.search_snippet_chars",
            context.search_snippet_chars,
            super::MAX_SEARCH_SNIPPET_CHARS,
        ),
        (
            "context_limits.self_tool_max_instructions_bytes",
            context.self_tool_max_instructions_bytes,
            super::MAX_SELF_TOOL_INSTRUCTIONS_BYTES,
        ),
        (
            "context_limits.self_tool_max_script_bytes",
            context.self_tool_max_script_bytes,
            super::MAX_SELF_TOOL_SCRIPT_BYTES,
        ),
        (
            "context_limits.mcp_max_binary_base64_bytes",
            context.mcp_max_binary_base64_bytes,
            super::MAX_MCP_MAX_BINARY_BASE64_BYTES,
        ),
        (
            "context_limits.clipboard_history_entries",
            context.clipboard_history_entries,
            super::MAX_CLIPBOARD_HISTORY_ENTRIES,
        ),
        (
            "context_limits.clipboard_history_max_entries",
            context.clipboard_history_max_entries,
            super::MAX_CLIPBOARD_HISTORY_ENTRIES,
        ),
        (
            "context_limits.clipboard_entry_max_chars",
            context.clipboard_entry_max_chars,
            super::MAX_CLIPBOARD_ENTRY_OUTPUT_CHARS,
        ),
    ] {
        if value > max {
            anyhow::bail!("{path} must not exceed {max}");
        }
    }
    if context.sanitize_field_max_chars == 0
        || context.sanitize_field_max_chars > super::MAX_SANITIZE_FIELD_CHARS
    {
        anyhow::bail!(
            "context_limits.sanitize_field_max_chars must be within 1..={}",
            super::MAX_SANITIZE_FIELD_CHARS
        );
    }
    if context.file_summary_timeout_secs == 0
        || context.file_summary_timeout_secs > super::MAX_FILE_SUMMARY_TIMEOUT_SECS
    {
        anyhow::bail!(
            "context_limits.file_summary_timeout_secs must be within 1..={} seconds",
            super::MAX_FILE_SUMMARY_TIMEOUT_SECS
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
    if !(super::MIN_SEARCH_MAX_RESULTS..=super::MAX_SEARCH_MAX_RESULTS)
        .contains(&context.search_max_results)
    {
        anyhow::bail!(
            "context_limits.search_max_results must be within {}..={}",
            super::MIN_SEARCH_MAX_RESULTS,
            super::MAX_SEARCH_MAX_RESULTS
        );
    }
    if context.search_window_bytes > super::MAX_SEARCH_WINDOW_BYTES {
        anyhow::bail!(
            "context_limits.search_window_bytes must not exceed {} bytes",
            super::MAX_SEARCH_WINDOW_BYTES
        );
    }
    if context.max_pending_scheduled_tool_runs > super::MAX_PENDING_SCHEDULED_TOOL_RUNS {
        anyhow::bail!(
            "context_limits.max_pending_scheduled_tool_runs must not exceed {}",
            super::MAX_PENDING_SCHEDULED_TOOL_RUNS
        );
    }
    if context.event_chunk_batch_max_bytes > super::MAX_EVENT_CHUNK_BATCH_MAX_BYTES {
        anyhow::bail!(
            "context_limits.event_chunk_batch_max_bytes must not exceed {} bytes",
            super::MAX_EVENT_CHUNK_BATCH_MAX_BYTES
        );
    }
    if context.scheduled_tool_runs_due_horizon_secs < 0
        || context.scheduled_tool_runs_due_horizon_secs
            > super::MAX_SCHEDULED_TOOL_RUNS_DUE_HORIZON_SECS
    {
        anyhow::bail!(
            "context_limits.scheduled_tool_runs_due_horizon_secs must be within 0..={} seconds",
            super::MAX_SCHEDULED_TOOL_RUNS_DUE_HORIZON_SECS
        );
    }
    if context.network_max_retries > super::MAX_NETWORK_RETRIES {
        anyhow::bail!(
            "context_limits.network_max_retries must not exceed {}",
            super::MAX_NETWORK_RETRIES
        );
    }
    if context.network_backoff_base_secs > super::MAX_NETWORK_BACKOFF_BASE_SECS {
        anyhow::bail!(
            "context_limits.network_backoff_base_secs must not exceed {} seconds",
            super::MAX_NETWORK_BACKOFF_BASE_SECS
        );
    }
    if !(super::MIN_EMBEDDING_CHUNK_SIZE..=super::MAX_EMBEDDING_CHUNK_SIZE)
        .contains(&context.embedding_chunk_size)
    {
        anyhow::bail!(
            "context_limits.embedding_chunk_size must be within {}..={}",
            super::MIN_EMBEDDING_CHUNK_SIZE,
            super::MAX_EMBEDDING_CHUNK_SIZE
        );
    }
    if !(super::MIN_MAX_TOOLS_PER_REQUEST..=super::MAX_TOOLS_PER_REQUEST)
        .contains(&context.max_tools_per_request)
    {
        anyhow::bail!(
            "context_limits.max_tools_per_request must be within {}..={}",
            super::MIN_MAX_TOOLS_PER_REQUEST,
            super::MAX_TOOLS_PER_REQUEST
        );
    }
    Ok(())
}

fn validate_skills_exec(config: &super::SkillsExecConfig) -> Result<()> {
    if !crate::path::is_absolute_local_path(&config.work_dir) {
        anyhow::bail!("skills_exec.work_dir must be an absolute local directory path");
    }
    if config.timeout_secs == 0 || config.timeout_secs > super::MAX_SKILLS_EXEC_TIMEOUT_SECS {
        anyhow::bail!(
            "skills_exec.timeout_secs must be within 1..={} seconds",
            super::MAX_SKILLS_EXEC_TIMEOUT_SECS
        );
    }
    if config.max_output_lines > super::MAX_SKILLS_EXEC_OUTPUT_LINES {
        anyhow::bail!(
            "skills_exec.max_output_lines must not exceed {}",
            super::MAX_SKILLS_EXEC_OUTPUT_LINES
        );
    }
    Ok(())
}

fn validate_context_windows(config: &AppConfig) -> Result<()> {
    let limits = &config.context_limits;
    if !(super::MIN_CONTEXT_WINDOW_TOKENS..=super::MAX_CONTEXT_WINDOW_TOKENS)
        .contains(&limits.default_context_window)
    {
        anyhow::bail!(
            "context_limits.default_context_window must be within {}..={} tokens",
            super::MIN_CONTEXT_WINDOW_TOKENS,
            super::MAX_CONTEXT_WINDOW_TOKENS
        );
    }
    if limits.response_token_floor > super::MAX_CONTEXT_WINDOW_TOKENS {
        anyhow::bail!(
            "context_limits.response_token_floor must not exceed {} tokens",
            super::MAX_CONTEXT_WINDOW_TOKENS
        );
    }
    if limits.compaction_reserve_tokens >= limits.default_context_window {
        anyhow::bail!(
            "context_limits.compaction_reserve_tokens must be smaller than context_limits.default_context_window"
        );
    }

    for (index, model) in config.llm.models.iter().enumerate() {
        let Some(window) = model.context_window else {
            continue;
        };
        if !(super::MIN_CONTEXT_WINDOW_TOKENS..=super::MAX_CONTEXT_WINDOW_TOKENS).contains(&window)
        {
            anyhow::bail!(
                "llm.models[{index}].context_window must be within {}..={} tokens",
                super::MIN_CONTEXT_WINDOW_TOKENS,
                super::MAX_CONTEXT_WINDOW_TOKENS
            );
        }
        if limits.compaction_reserve_tokens >= window {
            anyhow::bail!(
                "context_limits.compaction_reserve_tokens must be smaller than llm.models[{index}].context_window"
            );
        }
    }
    Ok(())
}

/// Validate SettingsSnapshot before accepting it from Tauri IPC. This uses the same
/// semantic rules as config persistence and rejects values that must be
/// staged through the secure credential commands.
pub fn validate_settings_contract(settings: &SettingsSnapshot) -> Result<()> {
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
            "credential values must be staged in secure storage before SettingsSnapshot can be saved"
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
/// config. This shared check applies equally to TOML loads and SettingsSnapshot edits.
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
    if !(1..=super::MAX_MCP_HEALTH_INTERVAL_SECS).contains(&config.health_interval_secs) {
        anyhow::bail!(
            "mcp_discovery.health_interval_secs must be within 1..={} seconds",
            super::MAX_MCP_HEALTH_INTERVAL_SECS
        );
    }
    if !(1..=super::MAX_MCP_RECONNECT_DELAY_MS).contains(&config.reconnect_initial_ms) {
        anyhow::bail!(
            "mcp_discovery.reconnect_initial_ms must be within 1..={} milliseconds",
            super::MAX_MCP_RECONNECT_DELAY_MS
        );
    }
    if !(1..=super::MAX_MCP_RECONNECT_DELAY_MS).contains(&config.reconnect_max_ms)
        || config.reconnect_max_ms < config.reconnect_initial_ms
    {
        anyhow::bail!(
            "mcp_discovery.reconnect_max_ms must be within 1..={} milliseconds and at least reconnect_initial_ms",
            super::MAX_MCP_RECONNECT_DELAY_MS
        );
    }
    if config.reconnect_max_retries > super::MAX_MCP_RECONNECT_RETRIES {
        anyhow::bail!(
            "mcp_discovery.reconnect_max_retries must not exceed {}",
            super::MAX_MCP_RECONNECT_RETRIES
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
    use crate::config::{
        AppConfig, IndependentSearchConfig, McpServerConfig, PerplexitySearchType, ProviderConfig,
        SettingsSnapshot,
    };

    type NumericConfigContractCase = (&'static str, u64, u64, fn(&mut AppConfig, u64));

    #[test]
    fn accepts_defaults_and_safe_integer_boundaries() {
        let mut config = AppConfig::default();
        config.context_limits.max_upload_total_bytes = 9_007_199_254_740_991;
        config.context_limits.default_context_window = super::super::MAX_CONTEXT_WINDOW_TOKENS;

        assert!(validate_app_config_numbers(&config).is_ok());
        assert!(validate_settings_numbers(&SettingsSnapshot::from(&config)).is_ok());
    }

    #[test]
    fn configured_execution_root_must_be_absolute() {
        let mut config = AppConfig::default();
        config.security.execution_root = Some(std::path::PathBuf::from("relative/project"));

        let error = validate_app_config(&config).unwrap_err();
        assert!(error.to_string().contains("security.execution_root"));
        assert!(validate_settings_contract(&SettingsSnapshot::from(&config)).is_err());
    }

    #[test]
    fn configured_skill_work_directory_must_be_absolute_and_local() {
        let mut config = AppConfig::default();
        config.skills_exec.work_dir = std::path::PathBuf::from("relative/skills-work");

        let error = validate_app_config(&config).unwrap_err();
        assert!(error.to_string().contains("skills_exec.work_dir"));
        assert!(validate_settings_contract(&SettingsSnapshot::from(&config)).is_err());
    }

    #[cfg(windows)]
    #[test]
    fn configured_skill_work_directory_rejects_unc_and_device_paths() {
        for path in [r"\\server\share\skills", r"\\?\C:\skills"] {
            let mut config = AppConfig::default();
            config.skills_exec.work_dir = std::path::PathBuf::from(path);

            let error = validate_app_config(&config).unwrap_err();
            assert!(error.to_string().contains("skills_exec.work_dir"));
        }
    }

    #[test]
    fn mcp_exposure_overrides_are_bounded_and_reject_control_patterns() {
        let mut config = AppConfig::default();
        let mut server = McpServerConfig {
            name: "calendar".into(),
            ..Default::default()
        };
        server
            .exposure
            .tools
            .insert("calendar_*".into(), crate::mcp::McpToolExposure::Direct);
        server
            .exposure
            .tools
            .insert("private_*".into(), crate::mcp::McpToolExposure::Hidden);
        config.mcp_servers.push(server.clone());
        assert!(validate_app_config(&config).is_ok());

        config.mcp_servers[0]
            .exposure
            .tools
            .insert("bad\npattern".into(), crate::mcp::McpToolExposure::Hidden);
        assert!(
            validate_app_config(&config)
                .unwrap_err()
                .to_string()
                .contains("non-control characters")
        );

        config.mcp_servers[0].exposure.tools.clear();
        for index in 0..=super::super::MAX_MCP_TOOL_EXPOSURE_OVERRIDES {
            config.mcp_servers[0].exposure.tools.insert(
                format!("tool_{index}"),
                crate::mcp::McpToolExposure::Deferred,
            );
        }
        assert!(
            validate_app_config(&config)
                .unwrap_err()
                .to_string()
                .contains("at most")
        );
    }

    #[test]
    fn context_window_and_compaction_reserve_share_one_budget() {
        let mut config = AppConfig::default();
        config.context_limits.default_context_window = super::super::MAX_CONTEXT_WINDOW_TOKENS;
        config.context_limits.compaction_reserve_tokens =
            super::super::MAX_CONTEXT_WINDOW_TOKENS - 1;
        config.llm.providers.push(ProviderConfig {
            name: "primary".into(),
            ..Default::default()
        });
        config.llm.models.push(crate::config::ModelConfig {
            id: "large-context".into(),
            provider_name: "primary".into(),
            model: "large-context-model".into(),
            context_window: Some(super::super::MAX_CONTEXT_WINDOW_TOKENS),
            ..Default::default()
        });
        assert!(validate_app_config(&config).is_ok());
        assert!(validate_settings_contract(&SettingsSnapshot::from(&config)).is_ok());

        for (invalid, path) in [
            (
                {
                    let mut config = AppConfig::default();
                    config.context_limits.default_context_window = u32::MAX;
                    config
                },
                "context_limits.default_context_window",
            ),
            (
                {
                    let mut config = AppConfig::default();
                    config.context_limits.compaction_reserve_tokens =
                        config.context_limits.default_context_window;
                    config
                },
                "compaction_reserve_tokens",
            ),
            (
                {
                    let mut config = AppConfig::default();
                    config.llm.models.push(crate::config::ModelConfig {
                        context_window: Some(999),
                        ..Default::default()
                    });
                    config
                },
                "llm.models[0].context_window",
            ),
            (
                {
                    let mut config = AppConfig::default();
                    config.llm.models.push(crate::config::ModelConfig {
                        context_window: Some(4_000),
                        ..Default::default()
                    });
                    config
                },
                "compaction_reserve_tokens",
            ),
        ] {
            let config_error = validate_app_config(&invalid).unwrap_err();
            assert!(config_error.to_string().contains(path), "{config_error}");
            let settings_error =
                validate_settings_contract(&SettingsSnapshot::from(&invalid)).unwrap_err();
            assert!(
                settings_error.to_string().contains(path),
                "{settings_error}"
            );
        }
    }

    #[test]
    fn independent_search_must_match_provider_and_documented_bounds() {
        let mut config = AppConfig::default();
        config.llm.providers.push(ProviderConfig {
            name: "primary".into(),
            provider: "deepseek".into(),
            ..Default::default()
        });
        config.llm.models.push(crate::config::ModelConfig {
            id: "search-model".into(),
            provider_name: "primary".into(),
            model: "deepseek-chat".into(),
            independent_search: Some(IndependentSearchConfig::DeepseekSearch {
                count: 1,
                need_content: false,
                pass_original_question: false,
                localization: None,
            }),
            ..Default::default()
        });
        assert!(validate_app_config(&config).is_ok());

        config.llm.models[0].independent_search = Some(IndependentSearchConfig::DeepseekSearch {
            count: 10,
            need_content: false,
            pass_original_question: true,
            localization: Some("not-an-ip".into()),
        });
        assert!(
            validate_app_config(&config)
                .unwrap_err()
                .to_string()
                .contains("localization")
        );

        config.llm.models[0].independent_search = Some(IndependentSearchConfig::DeepseekSearch {
            count: 51,
            need_content: false,
            pass_original_question: false,
            localization: None,
        });
        assert!(
            validate_app_config(&config)
                .unwrap_err()
                .to_string()
                .contains("independent_search.count")
        );

        config.llm.providers[0].provider = "perplexity".into();
        config.llm.models[0].independent_search = Some(IndependentSearchConfig::DeepseekSearch {
            count: 10,
            need_content: false,
            pass_original_question: false,
            localization: None,
        });
        assert!(
            validate_app_config(&config)
                .unwrap_err()
                .to_string()
                .contains("must match the selected Provider")
        );

        config.llm.models[0].independent_search = Some(IndependentSearchConfig::PerplexitySearch {
            max_results: 50,
            search_type: PerplexitySearchType::People,
            recency_filter: None,
            country: None,
            max_tokens: None,
            max_tokens_per_page: None,
            search_language_filter: Vec::new(),
            search_domain_filter: Vec::new(),
            last_updated_after_filter: None,
            last_updated_before_filter: None,
            search_after_date_filter: None,
            search_before_date_filter: None,
        });
        assert!(validate_app_config(&config).is_ok());

        config.llm.models[0].independent_search = Some(IndependentSearchConfig::PerplexitySearch {
            max_results: 10,
            search_type: PerplexitySearchType::Web,
            recency_filter: None,
            country: Some("us".into()),
            max_tokens: Some(10_000),
            max_tokens_per_page: Some(2_000),
            search_language_filter: vec!["en".into(), "zh".into()],
            search_domain_filter: vec!["wikipedia.org".into(), "-example.com".into()],
            last_updated_after_filter: Some("02/29/2024".into()),
            last_updated_before_filter: None,
            search_after_date_filter: Some("01/01/2024".into()),
            search_before_date_filter: None,
        });
        assert!(validate_app_config(&config).is_ok());
        if let Some(IndependentSearchConfig::PerplexitySearch {
            last_updated_after_filter,
            ..
        }) = config.llm.models[0].independent_search.as_mut()
        {
            *last_updated_after_filter = Some("02/30/2024".into());
        }
        assert!(
            validate_app_config(&config)
                .unwrap_err()
                .to_string()
                .contains("MM/DD/YYYY")
        );

        config.llm.models[0].independent_search = Some(IndependentSearchConfig::PerplexitySearch {
            max_results: 21,
            search_type: PerplexitySearchType::Web,
            recency_filter: None,
            country: None,
            max_tokens: None,
            max_tokens_per_page: None,
            search_language_filter: Vec::new(),
            search_domain_filter: Vec::new(),
            last_updated_after_filter: None,
            last_updated_before_filter: None,
            search_after_date_filter: None,
            search_before_date_filter: None,
        });
        assert!(
            validate_app_config(&config)
                .unwrap_err()
                .to_string()
                .contains("independent_search.max_results")
        );

        config.llm.providers[0].provider = "dashscope".into();
        config.llm.models[0].independent_search = Some(IndependentSearchConfig::AliyunAiSearch {
            endpoint: "https://opensearch.example".into(),
            workspace_name: "default".into(),
            top_k: 5,
            query_rewrite: true,
            content_type: crate::config::AliyunSearchContentType::Snippet,
            way: crate::config::AliyunSearchWay::Pro,
            time_range: crate::config::AliyunSearchTimeRange::NoLimit,
            api_key_ref: Some("cred-00000000000000000000000000000000".into()),
            api_key: String::new(),
        });
        assert!(validate_app_config(&config).is_ok());
        if let Some(IndependentSearchConfig::AliyunAiSearch { way, .. }) =
            config.llm.models[0].independent_search.as_mut()
        {
            *way = crate::config::AliyunSearchWay::ProFetch;
        }
        assert!(
            validate_app_config(&config)
                .unwrap_err()
                .to_string()
                .contains("requires content_type=mainText")
        );
        if let Some(IndependentSearchConfig::AliyunAiSearch { way, .. }) =
            config.llm.models[0].independent_search.as_mut()
        {
            *way = crate::config::AliyunSearchWay::Pro;
        }
        if let Some(IndependentSearchConfig::AliyunAiSearch { endpoint, .. }) =
            config.llm.models[0].independent_search.as_mut()
        {
            *endpoint = "http://opensearch.example".into();
        }
        assert!(
            validate_app_config(&config)
                .unwrap_err()
                .to_string()
                .contains("must be HTTPS")
        );
    }

    #[test]
    fn per_tool_output_cap_cannot_exceed_global_observation_budget() {
        let mut config = AppConfig::default();
        config.context_limits.max_observation_chars = 20_000;
        config.tool_settings.insert(
            "shell".into(),
            crate::config::ToolConfig {
                max_output_chars: Some(20_000),
                ..Default::default()
            },
        );
        assert!(validate_app_config(&config).is_ok());
        assert!(validate_settings_contract(&SettingsSnapshot::from(&config)).is_ok());

        config
            .tool_settings
            .get_mut("shell")
            .unwrap()
            .max_output_chars = Some(20_001);
        for validation in [
            validate_app_config(&config),
            validate_settings_contract(&SettingsSnapshot::from(&config)),
        ] {
            assert!(
                validation
                    .unwrap_err()
                    .to_string()
                    .contains("tool_settings.shell.max_output_chars")
            );
        }
    }

    #[test]
    fn media_timeout_ranges_match_settings_controls_for_config_and_settings() {
        let cases: [NumericConfigContractCase; 4] = [
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
                    validate_settings_contract(&SettingsSnapshot::from(&config)).is_ok(),
                    "SettingsSnapshot {path}={value}"
                );
            }

            for value in [0, min - 1, max + 1] {
                let mut config = AppConfig::default();
                set_value(&mut config, value);
                let config_error = validate_app_config(&config).unwrap_err();
                assert!(config_error.to_string().contains(path), "{config_error}");
                let settings_error =
                    validate_settings_contract(&SettingsSnapshot::from(&config)).unwrap_err();
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
            assert!(validate_settings_contract(&SettingsSnapshot::from(&config)).is_ok());
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
            let settings_error =
                validate_settings_contract(&SettingsSnapshot::from(&config)).unwrap_err();
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
            assert!(validate_settings_contract(&SettingsSnapshot::from(&config)).is_ok());
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
                validate_settings_contract(&SettingsSnapshot::from(&config))
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
            assert!(validate_settings_contract(&SettingsSnapshot::from(&config)).is_ok());
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
                validate_settings_contract(&SettingsSnapshot::from(&config))
                    .unwrap_err()
                    .to_string()
                    .contains(expected_path)
            );
        }
    }

    #[test]
    fn resource_bounds_match_config_and_settings_contracts() {
        let cases: [NumericConfigContractCase; 13] = [
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
                "context_limits.file_line_max_bytes",
                super::super::MIN_FILE_LINE_MAX_BYTES as u64,
                super::super::MAX_FILE_LINE_MAX_BYTES as u64,
                |config, value| config.context_limits.file_line_max_bytes = value as usize,
            ),
            (
                "context_limits.file_byte_read_max_bytes",
                super::super::MIN_FILE_BYTE_READ_MAX_BYTES,
                super::super::MAX_FILE_BYTE_READ_MAX_BYTES,
                |config, value| config.context_limits.file_byte_read_max_bytes = value,
            ),
            (
                "context_limits.max_known_facts",
                0,
                super::super::MAX_FACTS_IN_EXTRACTION_PROMPT as u64,
                |config, value| config.context_limits.max_known_facts = value as usize,
            ),
            (
                "context_limits.input_ring_buffer_secs",
                0,
                super::super::MAX_INPUT_RING_BUFFER_SECS as u64,
                |config, value| config.context_limits.input_ring_buffer_secs = value as usize,
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
            (
                "context_limits.search_max_results",
                super::super::MIN_SEARCH_MAX_RESULTS as u64,
                super::super::MAX_SEARCH_MAX_RESULTS as u64,
                |config, value| config.context_limits.search_max_results = value as usize,
            ),
            (
                "context_limits.search_window_bytes",
                0,
                super::super::MAX_SEARCH_WINDOW_BYTES,
                |config, value| config.context_limits.search_window_bytes = value,
            ),
            (
                "context_limits.embedding_chunk_size",
                super::super::MIN_EMBEDDING_CHUNK_SIZE as u64,
                super::super::MAX_EMBEDDING_CHUNK_SIZE as u64,
                |config, value| config.context_limits.embedding_chunk_size = value as usize,
            ),
            (
                "context_limits.max_tools_per_request",
                super::super::MIN_MAX_TOOLS_PER_REQUEST as u64,
                super::super::MAX_TOOLS_PER_REQUEST as u64,
                |config, value| config.context_limits.max_tools_per_request = value as usize,
            ),
        ];

        for (path, min, max, set_value) in cases {
            for value in [min, max] {
                let mut config = AppConfig::default();
                set_value(&mut config, value);
                assert!(validate_app_config(&config).is_ok(), "{path}={value}");
                assert!(
                    validate_settings_contract(&SettingsSnapshot::from(&config)).is_ok(),
                    "SettingsSnapshot {path}={value}"
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
                    validate_settings_contract(&SettingsSnapshot::from(&config))
                        .unwrap_err()
                        .to_string()
                        .contains(path),
                    "SettingsSnapshot {path}={value}"
                );
            }
        }
    }

    #[test]
    fn additional_resource_caps_match_config_and_settings_contracts() {
        let cases: [NumericConfigContractCase; 23] = [
            (
                "context_limits.max_observation_chars",
                0,
                super::super::MAX_CONTEXT_TEXT_CHARS as u64,
                |config, value| config.context_limits.max_observation_chars = value as usize,
            ),
            (
                "context_limits.max_transcript_chars",
                0,
                super::super::MAX_CONTEXT_TEXT_CHARS as u64,
                |config, value| config.context_limits.max_transcript_chars = value as usize,
            ),
            (
                "context_limits.tool_run_result_context_chars",
                0,
                super::super::MAX_CONTEXT_TEXT_CHARS as u64,
                |config, value| {
                    config.context_limits.tool_run_result_context_chars = value as usize
                },
            ),
            (
                "context_limits.notification_summary_chars",
                0,
                super::super::MAX_NOTIFICATION_SUMMARY_CHARS as u64,
                |config, value| config.context_limits.notification_summary_chars = value as usize,
            ),
            (
                "context_limits.file_max_full_file_bytes",
                0,
                super::super::MAX_FILE_OPERATION_BYTES,
                |config, value| config.context_limits.file_max_full_file_bytes = value,
            ),
            (
                "context_limits.file_write_max_bytes",
                1,
                super::super::MAX_FILE_OPERATION_BYTES,
                |config, value| config.context_limits.file_write_max_bytes = value,
            ),
            (
                "context_limits.file_vision_max_bytes",
                0,
                super::super::MAX_FILE_OPERATION_BYTES,
                |config, value| config.context_limits.file_vision_max_bytes = value,
            ),
            (
                "context_limits.file_summary_input_chars",
                0,
                super::super::MAX_FILE_SUMMARY_INPUT_CHARS as u64,
                |config, value| config.context_limits.file_summary_input_chars = value as usize,
            ),
            (
                "context_limits.search_snippet_chars",
                0,
                super::super::MAX_SEARCH_SNIPPET_CHARS as u64,
                |config, value| config.context_limits.search_snippet_chars = value as usize,
            ),
            (
                "context_limits.search_max_file_size_bytes",
                0,
                super::super::MAX_SEARCH_FILE_SIZE_BYTES,
                |config, value| config.context_limits.search_max_file_size_bytes = value,
            ),
            (
                "context_limits.self_tool_max_instructions_bytes",
                0,
                super::super::MAX_SELF_TOOL_INSTRUCTIONS_BYTES as u64,
                |config, value| {
                    config.context_limits.self_tool_max_instructions_bytes = value as usize
                },
            ),
            (
                "context_limits.self_tool_max_script_bytes",
                0,
                super::super::MAX_SELF_TOOL_SCRIPT_BYTES as u64,
                |config, value| config.context_limits.self_tool_max_script_bytes = value as usize,
            ),
            (
                "context_limits.mcp_max_binary_base64_bytes",
                0,
                super::super::MAX_MCP_MAX_BINARY_BASE64_BYTES as u64,
                |config, value| config.context_limits.mcp_max_binary_base64_bytes = value as usize,
            ),
            (
                "context_limits.clipboard_history_entries",
                0,
                super::super::MAX_CLIPBOARD_HISTORY_ENTRIES as u64,
                |config, value| config.context_limits.clipboard_history_entries = value as usize,
            ),
            (
                "context_limits.clipboard_history_max_entries",
                0,
                super::super::MAX_CLIPBOARD_HISTORY_ENTRIES as u64,
                |config, value| {
                    config.context_limits.clipboard_history_max_entries = value as usize
                },
            ),
            (
                "context_limits.clipboard_entry_max_chars",
                0,
                super::super::MAX_CLIPBOARD_ENTRY_OUTPUT_CHARS as u64,
                |config, value| config.context_limits.clipboard_entry_max_chars = value as usize,
            ),
            (
                "context_limits.max_pending_scheduled_tool_runs",
                0,
                super::super::MAX_PENDING_SCHEDULED_TOOL_RUNS as u64,
                |config, value| {
                    config.context_limits.max_pending_scheduled_tool_runs = value as usize
                },
            ),
            (
                "context_limits.event_chunk_batch_max_bytes",
                0,
                super::super::MAX_EVENT_CHUNK_BATCH_MAX_BYTES as u64,
                |config, value| config.context_limits.event_chunk_batch_max_bytes = value as usize,
            ),
            (
                "context_limits.network_max_retries",
                0,
                super::super::MAX_NETWORK_RETRIES as u64,
                |config, value| config.context_limits.network_max_retries = value as u32,
            ),
            (
                "context_limits.network_backoff_base_secs",
                0,
                super::super::MAX_NETWORK_BACKOFF_BASE_SECS,
                |config, value| config.context_limits.network_backoff_base_secs = value,
            ),
            (
                "mcp_discovery.health_interval_secs",
                1,
                super::super::MAX_MCP_HEALTH_INTERVAL_SECS,
                |config, value| config.mcp_discovery.health_interval_secs = value,
            ),
            (
                "mcp_discovery.reconnect_initial_ms",
                1,
                super::super::MAX_MCP_RECONNECT_DELAY_MS,
                |config, value| config.mcp_discovery.reconnect_initial_ms = value,
            ),
            (
                "mcp_discovery.reconnect_max_ms",
                1,
                super::super::MAX_MCP_RECONNECT_DELAY_MS,
                |config, value| config.mcp_discovery.reconnect_max_ms = value,
            ),
        ];

        for (path, min, max, set_value) in cases {
            for value in [min, max] {
                let mut config = AppConfig::default();
                config.mcp_discovery.reconnect_initial_ms = 1;
                config.mcp_discovery.reconnect_max_ms = super::super::MAX_MCP_RECONNECT_DELAY_MS;
                set_value(&mut config, value);
                assert!(validate_app_config(&config).is_ok(), "{path}={value}");
                assert!(
                    validate_settings_contract(&SettingsSnapshot::from(&config)).is_ok(),
                    "SettingsSnapshot {path}={value}"
                );
            }

            for value in min.checked_sub(1).into_iter().chain([max + 1]) {
                let mut config = AppConfig::default();
                config.mcp_discovery.reconnect_initial_ms = 1;
                config.mcp_discovery.reconnect_max_ms = super::super::MAX_MCP_RECONNECT_DELAY_MS;
                set_value(&mut config, value);
                assert!(
                    validate_app_config(&config)
                        .unwrap_err()
                        .to_string()
                        .contains(path),
                    "config {path}={value}"
                );
                assert!(
                    validate_settings_contract(&SettingsSnapshot::from(&config))
                        .unwrap_err()
                        .to_string()
                        .contains(path),
                    "SettingsSnapshot {path}={value}"
                );
            }
        }
    }

    #[test]
    fn executor_and_schedule_budgets_reject_unbounded_values() {
        for (timeout, lines, valid) in [
            (1, 0, true),
            (
                super::super::MAX_SKILLS_EXEC_TIMEOUT_SECS,
                super::super::MAX_SKILLS_EXEC_OUTPUT_LINES,
                true,
            ),
            (0, 5_000, false),
            (super::super::MAX_SKILLS_EXEC_TIMEOUT_SECS + 1, 5_000, false),
            (30, super::super::MAX_SKILLS_EXEC_OUTPUT_LINES + 1, false),
        ] {
            let mut config = AppConfig::default();
            config.skills_exec.timeout_secs = timeout;
            config.skills_exec.max_output_lines = lines;
            assert_eq!(validate_app_config(&config).is_ok(), valid);
            assert_eq!(
                validate_settings_contract(&SettingsSnapshot::from(&config)).is_ok(),
                valid
            );
        }

        for horizon in [0, super::super::MAX_SCHEDULED_TOOL_RUNS_DUE_HORIZON_SECS] {
            let mut config = AppConfig::default();
            config.context_limits.scheduled_tool_runs_due_horizon_secs = horizon;
            assert!(validate_app_config(&config).is_ok());
            assert!(validate_settings_contract(&SettingsSnapshot::from(&config)).is_ok());
        }
        for horizon in [
            -1,
            super::super::MAX_SCHEDULED_TOOL_RUNS_DUE_HORIZON_SECS + 1,
        ] {
            let mut config = AppConfig::default();
            config.context_limits.scheduled_tool_runs_due_horizon_secs = horizon;
            assert!(validate_app_config(&config).is_err());
            assert!(validate_settings_contract(&SettingsSnapshot::from(&config)).is_err());
        }

        for (retries, valid) in [
            (0, true),
            (super::super::MAX_MCP_RECONNECT_RETRIES, true),
            (super::super::MAX_MCP_RECONNECT_RETRIES + 1, false),
        ] {
            let mut config = AppConfig::default();
            config.mcp_discovery.reconnect_max_retries = retries;
            assert_eq!(validate_app_config(&config).is_ok(), valid);
            assert_eq!(
                validate_settings_contract(&SettingsSnapshot::from(&config)).is_ok(),
                valid
            );
        }

        let mut config = AppConfig::default();
        config.tool_settings.insert(
            "shell".into(),
            crate::config::ToolConfig {
                timeout_secs: Some(super::super::MAX_TOOL_TIMEOUT_SECS),
                max_retries: Some(super::super::MAX_TOOL_RETRIES),
                retry_backoff_secs: Some(super::super::MAX_TOOL_RETRY_BACKOFF_SECS),
                ..Default::default()
            },
        );
        assert!(validate_app_config(&config).is_ok());
        assert!(validate_settings_contract(&SettingsSnapshot::from(&config)).is_ok());
        config.tool_settings.get_mut("shell").unwrap().timeout_secs =
            Some(super::super::MAX_TOOL_TIMEOUT_SECS + 1);
        assert!(
            validate_app_config(&config)
                .unwrap_err()
                .to_string()
                .contains("tool_settings.shell.timeout_secs")
        );
        assert!(
            validate_settings_contract(&SettingsSnapshot::from(&config))
                .unwrap_err()
                .to_string()
                .contains("tool_settings.shell.timeout_secs")
        );
    }

    #[test]
    fn history_retention_matches_settings_range_and_zero_semantics() {
        for days in [0, super::super::MAX_HISTORY_RETENTION_DAYS] {
            let mut config = AppConfig::default();
            config.session.history_retention_days = days;
            assert!(validate_app_config(&config).is_ok());
            assert!(validate_settings_contract(&SettingsSnapshot::from(&config)).is_ok());
        }

        let mut config = AppConfig::default();
        config.session.history_retention_days = super::super::MAX_HISTORY_RETENTION_DAYS + 1;
        assert!(
            validate_app_config(&config)
                .unwrap_err()
                .to_string()
                .contains("history_retention_days")
        );
        assert!(validate_settings_contract(&SettingsSnapshot::from(&config)).is_err());
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
        let mut settings = SettingsSnapshot::default();
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
        let settings_error =
            validate_settings_contract(&SettingsSnapshot::from(&config)).unwrap_err();
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
        assert!(validate_settings_contract(&SettingsSnapshot::from(&config)).is_err());

        config.llm.providers[0].api_style = None;
        config.media.tts.provider_name = "missing".into();
        assert!(validate_app_config(&config).is_err());
        assert!(validate_settings_contract(&SettingsSnapshot::from(&config)).is_err());

        let mut config = AppConfig::default();
        config.llm.providers.push(ProviderConfig {
            name: "chat-only".into(),
            api_style: Some("anthropic".into()),
            media_capabilities: crate::config::ProviderMediaCapabilities {
                tts: true,
                ..Default::default()
            },
            ..Default::default()
        });
        assert!(
            validate_app_config(&config)
                .unwrap_err()
                .to_string()
                .contains("media_capabilities")
        );
        assert!(validate_settings_contract(&SettingsSnapshot::from(&config)).is_err());
    }

    #[test]
    fn media_and_web_search_semantics_match_config_and_settings_contracts() {
        let mut config = AppConfig::default();
        config.llm.providers.push(ProviderConfig {
            name: "primary".into(),
            default_web_search: Some(" ReQuIrEd ".into()),
            ..Default::default()
        });
        config.llm.models.push(crate::config::ModelConfig {
            id: "chat".into(),
            provider_name: "primary".into(),
            model: "model".into(),
            web_search: Some("TRUE".into()),
            ..Default::default()
        });
        assert!(validate_app_config(&config).is_ok());
        assert!(validate_settings_contract(&SettingsSnapshot::from(&config)).is_ok());

        for provider_name in ["", "none", "NONE", " primary "] {
            let mut config = AppConfig::default();
            config.llm.providers.push(ProviderConfig {
                name: "primary".into(),
                ..Default::default()
            });
            config.media.tts.provider_name = provider_name.into();
            config.media.image_gen.provider_name = provider_name.into();
            assert!(
                validate_app_config(&config).is_ok(),
                "media provider {provider_name:?}"
            );
            assert!(validate_settings_contract(&SettingsSnapshot::from(&config)).is_ok());
        }

        let mut config = AppConfig::default();
        config.media.tts.provider_name = "missing".into();
        assert!(
            validate_app_config(&config)
                .unwrap_err()
                .to_string()
                .contains("media.tts.provider_name")
        );
        assert!(validate_settings_contract(&SettingsSnapshot::from(&config)).is_err());
        config.media.tts.provider_name = "none".into();
        config.media.image_gen.provider_name = "missing".into();
        assert!(
            validate_app_config(&config)
                .unwrap_err()
                .to_string()
                .contains("media.image_gen.provider_name")
        );
        assert!(validate_settings_contract(&SettingsSnapshot::from(&config)).is_err());

        for provider in ["llm", "baidu", "azure", "tencent", "none"] {
            let mut config = AppConfig::default();
            config.media.ocr.provider = provider.into();
            assert!(
                validate_app_config(&config).is_ok(),
                "OCR provider {provider}"
            );
            assert!(
                validate_settings_contract(&SettingsSnapshot::from(&config)).is_ok(),
                "SettingsSnapshot OCR provider {provider}"
            );
        }

        let mut config = AppConfig::default();
        config.media.ocr.provider = "OCR-LATER".into();
        assert!(
            validate_app_config(&config)
                .unwrap_err()
                .to_string()
                .contains("media.ocr.provider")
        );
        assert!(validate_settings_contract(&SettingsSnapshot::from(&config)).is_err());

        for mode in [
            "off", "auto", "always", "required", "on", "1", "true", " TRUE ", "  ",
        ] {
            let mut config = AppConfig::default();
            config.llm.providers.push(ProviderConfig {
                name: "primary".into(),
                default_web_search: Some(mode.into()),
                ..Default::default()
            });
            assert!(
                validate_app_config(&config).is_ok(),
                "provider mode {mode:?}"
            );
            assert!(validate_settings_contract(&SettingsSnapshot::from(&config)).is_ok());
            config.llm.providers[0].default_web_search = None;
            config.llm.models.push(crate::config::ModelConfig {
                id: "chat".into(),
                provider_name: "primary".into(),
                model: "model".into(),
                web_search: Some(mode.into()),
                ..Default::default()
            });
            assert!(validate_app_config(&config).is_ok(), "model mode {mode:?}");
            assert!(validate_settings_contract(&SettingsSnapshot::from(&config)).is_ok());
        }

        let mut config = AppConfig::default();
        config.llm.providers.push(ProviderConfig {
            name: "primary".into(),
            default_web_search: Some("alway".into()),
            ..Default::default()
        });
        assert!(
            validate_app_config(&config)
                .unwrap_err()
                .to_string()
                .contains("default_web_search")
        );
        assert!(validate_settings_contract(&SettingsSnapshot::from(&config)).is_err());
        config.llm.providers[0].default_web_search = None;
        config.llm.models.push(crate::config::ModelConfig {
            id: "chat".into(),
            provider_name: "primary".into(),
            model: "model".into(),
            web_search: Some("alway".into()),
            ..Default::default()
        });
        assert!(
            validate_app_config(&config)
                .unwrap_err()
                .to_string()
                .contains("web_search")
        );
        assert!(validate_settings_contract(&SettingsSnapshot::from(&config)).is_err());

        let mut config = AppConfig::default();
        config.media.stt.provider_name = "LLM".into();
        assert!(validate_app_config(&config).is_ok());
        assert!(validate_settings_contract(&SettingsSnapshot::from(&config)).is_ok());
        config.media.stt.provider_name = "MCP".into();
        assert!(validate_app_config(&config).is_err());
        config.mcp_servers.push(McpServerConfig {
            name: "transcriber".into(),
            ..Default::default()
        });
        config.media.stt.mcp_server = Some("transcriber".into());
        assert!(validate_app_config(&config).is_ok());
        assert!(validate_settings_contract(&SettingsSnapshot::from(&config)).is_ok());
        config.media.stt.mcp_server = Some("missing".into());
        assert!(
            validate_app_config(&config)
                .unwrap_err()
                .to_string()
                .contains("mcp_server")
        );
        assert!(validate_settings_contract(&SettingsSnapshot::from(&config)).is_err());
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
            validate_settings_contract(&SettingsSnapshot::from(&config)),
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
            validate_settings_contract(&SettingsSnapshot::from(&config)),
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
            validate_settings_contract(&SettingsSnapshot::from(&config)),
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

        let mut settings = SettingsSnapshot::from(&config);
        settings.llm.models[0].id = "  ".into();
        assert!(
            validate_settings_contract(&settings)
                .unwrap_err()
                .to_string()
                .contains("llm.models[0].id")
        );

        let mut settings = SettingsSnapshot::from(&config);
        settings.llm.models[0].model = "  ".into();
        assert!(
            validate_settings_contract(&settings)
                .unwrap_err()
                .to_string()
                .contains("llm.models[0].model")
        );

        let mut settings = SettingsSnapshot::from(&config);
        settings.llm.models.push(settings.llm.models[0].clone());
        assert!(
            validate_settings_contract(&settings)
                .unwrap_err()
                .to_string()
                .contains("llm.models[1].id must be unique")
        );
    }
}
