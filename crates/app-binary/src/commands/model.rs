use crate::app_state::AppState;
use crate::commands::log_err;
use haven_common::config::{
    AppConfig, LlmConfig, ModelConfig, ProviderConfig, RequestKind, provider_config_wire_style,
};
use haven_llm::ModelInfo;
use haven_llm::ModelRegistry;
use std::collections::BTreeMap;
use std::sync::Arc;
use tauri::Manager;
use tauri::State;

/// Resolve a model id or `RequestKind` string to a named model, or `None` for
/// an unknown value. This is the single selector boundary for the model
/// parameter commands (`set_reasoning_effort`, `set_web_search`).
fn model_id_for_selector(cfg: &LlmConfig, model_id_or_request_kind: &str) -> Option<String> {
    if cfg.model(model_id_or_request_kind).is_some() {
        return Some(model_id_or_request_kind.to_string());
    }
    let request = RequestKind::from_str(model_id_or_request_kind)?;
    cfg.policy(request).map(|policy| policy.primary.clone())
}

fn model_slot<'a>(
    cfg: &'a mut LlmConfig,
    model_id_or_request_kind: &str,
) -> Option<&'a mut ModelConfig> {
    let id = model_id_for_selector(cfg, model_id_or_request_kind)?;
    cfg.model_mut(&id)
}

fn set_request_route(
    llm: &mut LlmConfig,
    request_name: &str,
    model_config_id: &str,
) -> Result<(), String> {
    let request = RequestKind::from_str(request_name)
        .ok_or_else(|| format!("unknown request kind: {request_name}"))?;
    let model = llm
        .model(model_config_id)
        .ok_or_else(|| format!("unknown model configuration: {model_config_id}"))?;
    if !model.is_assigned() || llm.provider(&model.provider).is_none() {
        return Err(format!(
            "model configuration is incomplete: {model_config_id}"
        ));
    }
    if !model.capabilities.contains(&request.required_capability()) {
        return Err(format!(
            "model configuration {model_config_id} does not support {}",
            request.as_str()
        ));
    }

    llm.set_policy(request, model_config_id);
    Ok(())
}

fn validate_builtin_search(config: &AppConfig, selector: &str) -> Result<(), String> {
    let llm = &config.llm;
    let model_id = model_id_for_selector(llm, selector)
        .ok_or_else(|| format!("unknown or unconfigured model/request: {}", selector))?;
    let slot = llm
        .model(&model_id)
        .ok_or_else(|| format!("unknown or unconfigured model/request: {}", selector))?;
    let style = llm
        .providers
        .iter()
        .find(|provider| provider.name == slot.provider)
        .map(provider_config_wire_style)
        .unwrap_or("openai-chat");
    if !haven_llm::supports_builtin_web_search(style) {
        return Err(format!(
            "provider wire style `{style}` does not support built-in search"
        ));
    }
    Ok(())
}

/// Normalize an endpoint URL for comparison: strip the trailing slash and
/// lowercase it (scheme/host comparisons are case-insensitive).
fn normalize_endpoint_url(url: &str) -> String {
    url.trim_end_matches('/').to_ascii_lowercase()
}

/// Infer an STT-only catalog from a base URL host (used when the provider is
/// not yet persisted in config.toml, e.g. right after the settings dialog).
fn stt_only_catalog_for_url(base_url: &str) -> Option<Vec<ModelInfo>> {
    let host = base_url.to_ascii_lowercase();
    if host.contains("deepgram") {
        stt_only_catalog(Some("deepgram"))
    } else if host.contains("assemblyai") {
        stt_only_catalog(Some("assemblyai"))
    } else {
        None
    }
}

/// Static model catalog for STT-only providers that have no `/models` list.
fn stt_only_catalog(api_style: Option<&str>) -> Option<Vec<ModelInfo>> {
    let raw = api_style?;
    if !haven_llm::is_stt_only_style(raw) {
        return None;
    }
    let style = haven_llm::normalize_api_style(raw);
    let (provider, models): (&str, &[&str]) = match style {
        "deepgram" => (
            "deepgram",
            &[
                "nova-3",
                "nova-2",
                "whisper-large-v3",
                "whisper-large-v3-turbo",
            ],
        ),
        "assemblyai" => (
            "assemblyai",
            &[
                "assemblyai_default",
                "universal",
                "universal-2",
                "universal-3-pro",
            ],
        ),
        _ => return None,
    };
    Some(
        models
            .iter()
            .map(|id| ModelInfo::bare(*id, provider))
            .collect(),
    )
}

/// The auth scheme a provider uses for model discovery and chat: an explicit
/// `x-api-key` / `x-goog-api-key` style (customized header or the Anthropic /
/// Gemini wire protocol) or the OpenAI-style `Authorization: Bearer`.
fn provider_auth_scheme(p: &ProviderConfig) -> (String, String) {
    let customized = p.auth_header_name != "Authorization" || p.auth_header_prefix != "Bearer";
    if customized {
        (p.auth_header_name.clone(), p.auth_header_prefix.clone())
    } else {
        match p.api_style.as_deref().map(haven_llm::normalize_api_style) {
            Some("anthropic") => ("x-api-key".to_string(), String::new()),
            Some("gemini") => ("x-goog-api-key".to_string(), String::new()),
            _ => ("Authorization".to_string(), "Bearer".to_string()),
        }
    }
}

/// Build the `Authorization`-style value. A `None` prefix means the key is
/// sent raw (Anthropic / Gemini API keys).
fn auth_value(prefix: &str, key: &str) -> String {
    if prefix.is_empty() {
        key.to_string()
    } else {
        format!("{} {}", prefix, key)
    }
}

/// Resolve the api key and auth scheme for a model-list fetch.
///
/// - An explicit `api_key` wins; a request may provide the auth header scheme
///   from a selected, user-entered provider preset. Without that override, the
///   matching configured provider is used, falling back to OpenAI-style Bearer.
/// - An empty `api_key` falls back to the named provider's stored key, guarded
///   by URL: the provider is only used when its configured base URL matches
///   the requested one, so a stored key can never be sent to an arbitrary
///   renderer-supplied host.
fn resolve_discovery_auth(
    cfg: &AppConfig,
    base_url: &str,
    api_key: &str,
    provider: Option<&str>,
    auth_header_name: Option<&str>,
    auth_header_prefix: Option<&str>,
) -> Option<(String, (String, String))> {
    let requested = normalize_endpoint_url(base_url);
    let provider_cfg = provider.and_then(|name| cfg.llm.provider(name));

    if !api_key.is_empty() {
        let (h, pfx) = if let Some(header_name) = auth_header_name.filter(|name| !name.is_empty()) {
            (
                header_name.to_string(),
                auth_header_prefix.unwrap_or_default().to_string(),
            )
        } else {
            provider_cfg
                .filter(|p| normalize_endpoint_url(&p.base_url) == requested)
                .map(provider_auth_scheme)
                .unwrap_or_else(|| ("Authorization".to_string(), "Bearer".to_string()))
        };
        let value = auth_value(&pfx, api_key);
        return Some((api_key.to_string(), (h, value)));
    }

    if let Some(p) = provider_cfg.filter(|p| normalize_endpoint_url(&p.base_url) == requested) {
        let (h, pfx) = provider_auth_scheme(p);
        let value = auth_value(&pfx, &p.api_key);
        return Some((p.api_key.clone(), (h, value)));
    }
    None
}

/// True when the settings UI should treat a provider as configured.
/// Delegates to [`haven_common::config::provider_credentials_ready`] so UI
/// status and runtime `LlmConfig::is_configured` stay aligned.
fn provider_is_configured(p: &ProviderConfig) -> bool {
    haven_common::config::provider_credentials_ready(p)
}

/// STT key status is derived only from the selected named provider.
fn stt_key_configured(stt: &haven_common::config::SttConfig, providers: &[ProviderConfig]) -> bool {
    let name = stt.provider.trim();
    if name.is_empty() || name.eq_ignore_ascii_case("none") || name == "llm" || name == "mcp" {
        return false;
    }
    if let Some(p) = providers.iter().find(|p| p.name == name) {
        return provider_is_configured(p);
    }
    false
}

/// Build the `{models: {id: bool}, providers: {name: bool}, stt/ocr/...}` payload the
/// settings page uses for StatusDot / Set vs Change. Reads the live config
/// (same snapshot as model discovery), not a fresh disk reload.
/// TTS / image-gen reuse `providers` credentials, so they have no separate
/// key flags here.
#[derive(Debug, serde::Serialize)]
pub struct ApiKeyStatus {
    pub models: BTreeMap<String, bool>,
    pub providers: BTreeMap<String, bool>,
    pub stt: bool,
    pub ocr: bool,
    pub ocr_secret: bool,
}

fn api_key_status(cfg: &AppConfig) -> ApiKeyStatus {
    let mut providers = BTreeMap::new();
    for p in &cfg.llm.providers {
        providers.insert(p.name.clone(), provider_is_configured(p));
    }
    let mut models = BTreeMap::new();
    for model in &cfg.llm.models {
        let configured = model.is_assigned()
            && cfg
                .llm
                .provider(&model.provider)
                .is_some_and(haven_common::config::provider_credentials_ready);
        models.insert(model.id.clone(), configured);
    }
    ApiKeyStatus {
        models,
        providers,
        stt: stt_key_configured(&cfg.media.stt, &cfg.llm.providers),
        ocr: !cfg.media.ocr.api_key.is_empty(),
        ocr_secret: !cfg.media.ocr.api_secret.is_empty(),
    }
}

#[tauri::command]
pub async fn get_api_key_status(app: tauri::AppHandle) -> Result<ApiKeyStatus, String> {
    let state = app.state::<Arc<AppState>>();
    let cfg = state
        .config_service
        .snapshot()
        .map_err(|e| log_err("get_api_key_status", e))?
        .config;
    Ok(api_key_status(&cfg))
}

/// Probe the configured default-model endpoint for live connectivity.
/// Returns a typed status plus a non-sensitive failure category. Detailed
/// transport causes are retained in the backend log, never returned with the
/// Tauri response.
#[tauri::command]
pub async fn check_llm_connection(
    state: State<'_, Arc<AppState>>,
) -> Result<haven_llm::LlmConnectionReport, String> {
    Ok(state.agent.check_llm_connection().await)
}

/// Resolve the auth scheme (header name, prefix) for an STT provider during
/// model discovery. Gemini uses its `x-goog-api-key` wire scheme; every other
/// provider goes through the OpenAI-style `Authorization: Bearer`.
fn stt_auth_scheme(provider: &str) -> (String, String) {
    if provider == "gemini" {
        ("x-goog-api-key".to_string(), String::new())
    } else {
        ("Authorization".to_string(), "Bearer".to_string())
    }
}

/// §2.7: Fetch models from a provider's `/models` endpoint (OpenAI-
/// compatible). Used by the settings UI to populate the model dropdown after
/// a provider's base URL and API key are entered (or refreshed with a stored
/// key). The auth scheme follows the provider's wire protocol (Anthropic /
/// Gemini / custom auth header), not just OpenAI-style Bearer.
///
/// When `api_key` is empty (masked) and `provider` names a configured provider
/// whose base URL matches `base_url`, the stored key is used — never sent to
/// an arbitrary renderer-supplied host. `skip_auth` is reserved for explicit
/// keyless provider presets. `role = "transcription"` resolves
/// through the `media.stt` config instead (STT model discovery). The IPC key
/// remains `role` for the existing UI, but its value is a model id or a
/// [`RequestKind`] string.
// Tauri derives the typed flat camelCase IPC request from this signature; keep
// the optional auth scheme fields explicit at that boundary.
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn discover_models(
    base_url: String,
    api_key: String,
    provider: Option<String>,
    role: Option<String>,
    auth_header_name: Option<String>,
    auth_header_prefix: Option<String>,
    skip_auth: Option<bool>,
    proxy_url: Option<String>,
    no_proxy: Option<String>,
    app: tauri::AppHandle,
) -> Result<Vec<ModelInfo>, String> {
    if !base_url.starts_with("http://") && !base_url.starts_with("https://") {
        return Err(log_err(
            "discover_models",
            "base_url must be an http(s) URL",
        ));
    }
    let state = app.state::<Arc<AppState>>();
    let cfg = state
        .config_service
        .snapshot()
        .map_err(|e| log_err("discover_models", e))?
        .config;

    // STT-only providers (Deepgram / AssemblyAI) have no `/models` endpoint;
    // return the static catalog so the model picker can still assign a model.
    // The selected STT value is always a name from `llm.providers`; unsaved
    // discovery can still use the requested URL host below.
    if let Some(name) = provider.as_deref().filter(|n| !n.is_empty())
        && let Some(p) = cfg.llm.provider(name)
        && let Some(list) = stt_only_catalog(p.api_style.as_deref())
    {
        return Ok(list);
    }
    if let Some(list) = stt_only_catalog_for_url(&base_url) {
        return Ok(list);
    }

    let key_and_auth = if api_key.is_empty() && skip_auth.unwrap_or(false) {
        Some((String::new(), None))
    } else if role.as_deref().and_then(RequestKind::from_str) == Some(RequestKind::Transcription) {
        // STT discovery: prefer an explicit key, otherwise use only the named
        // `llm.providers` entry selected by the request or media settings.
        let stt = &cfg.media.stt;
        let requested = normalize_endpoint_url(&base_url);
        if !api_key.is_empty() {
            let scheme_name = provider
                .as_deref()
                .filter(|n| !n.is_empty())
                .unwrap_or(stt.provider.as_str());
            let backend = cfg
                .llm
                .provider(scheme_name)
                .map(|p| p.provider.as_str())
                .unwrap_or(scheme_name);
            let (h, pfx) = stt_auth_scheme(backend);
            let value = auth_value(&pfx, &api_key);
            Some((api_key.clone(), Some((h, value))))
        } else if let Some(name) = provider
            .as_deref()
            .filter(|n| !n.is_empty())
            .or(Some(stt.provider.as_str()))
            .filter(|n| {
                !matches!(
                    *n,
                    "none"
                        | "llm"
                        | "mcp"
                        | "openai"
                        | "groq"
                        | "gemini"
                        | "deepgram"
                        | "assemblyai"
                )
            })
            && let Some(p) = cfg.llm.provider(name)
            && normalize_endpoint_url(&p.base_url) == requested
        {
            let (h, pfx) = stt_auth_scheme(&p.provider);
            let value = auth_value(&pfx, &p.api_key);
            Some((p.api_key.clone(), Some((h, value))))
        } else {
            None
        }
    } else {
        resolve_discovery_auth(
            &cfg,
            &base_url,
            &api_key,
            provider.as_deref(),
            auth_header_name.as_deref(),
            auth_header_prefix.as_deref(),
        )
        .map(|(key, auth)| (key, Some(auth)))
    };

    let (key, auth) = key_and_auth.ok_or_else(|| {
        "未找到可用的 API Key：请填写 API Key，或先保存 Provider 配置（其 Base URL 需与请求地址一致）"
            .to_string()
    })?;

    let mut reg = ModelRegistry::new();
    tracing::info!(
        endpoint_host = %haven_llm::endpoint_host(&base_url),
        provider = provider.as_deref().unwrap_or("unknown"),
        "discovering models"
    );
    let models = reg
        .discover_from_with_proxy(
            &base_url,
            &key,
            auth.as_ref()
                .map(|(header, value)| (header.as_str(), value.as_str())),
            proxy_url.as_deref(),
            no_proxy.as_deref(),
        )
        .await
        .map_err(|e| {
            tracing::warn!(
                endpoint_host = %haven_llm::endpoint_host(&base_url),
                provider = provider.as_deref().unwrap_or("unknown"),
                reason = e.connection_failure_reason().as_str(),
                error = %haven_common::error::sanitize_error_text(&e.to_string()),
                "model discovery failed"
            );
            log_err("discover_models", &e)
        })?;
    tracing::info!(
        endpoint_host = %haven_llm::endpoint_host(&base_url),
        provider = provider.as_deref().unwrap_or("unknown"),
        model_count = models.len(),
        "model discovery completed"
    );
    Ok(models)
}

/// Fetch the model lists for every configured LLM provider in parallel,
/// returning `{ provider_name: [ModelInfo] }`. Used by the settings UI to
/// cache provider model lists (auto-refreshed on load + a manual refresh
/// button); providers without a stored key or that fail to respond simply
/// yield an empty list.
#[tauri::command]
pub async fn discover_all_models(
    app: tauri::AppHandle,
) -> Result<BTreeMap<String, Vec<ModelInfo>>, String> {
    let state = app.state::<Arc<AppState>>();
    let cfg = state
        .config_service
        .snapshot()
        .map_err(|e| log_err("discover_all_models", e))?
        .config;
    let providers = cfg.llm.providers.clone();
    let mut handles = Vec::new();
    let mut results = BTreeMap::new();
    for p in &providers {
        if p.base_url.is_empty() || !provider_is_configured(p) {
            continue;
        }
        if let Some(list) = stt_only_catalog(p.api_style.as_deref()) {
            results.insert(p.name.clone(), list);
            continue;
        }
        let (header, prefix) = provider_auth_scheme(p);
        let name = p.name.clone();
        let base_url = p.base_url.clone();
        let api_key = p.api_key.clone();
        let proxy_url = p.proxy_url.clone();
        let no_proxy = p.no_proxy.clone();
        let auth_header = if api_key.is_empty() {
            None
        } else {
            Some((header, auth_value(&prefix, &api_key)))
        };
        handles.push(tokio::spawn(async move {
            let mut reg = ModelRegistry::new();
            let auth_ref = auth_header.as_ref().map(|(h, v)| (h.as_str(), v.as_str()));
            match reg
                .discover_from_with_proxy(
                    &base_url,
                    &api_key,
                    auth_ref,
                    proxy_url.as_deref(),
                    no_proxy.as_deref(),
                )
                .await
            {
                Ok(list) => (name.clone(), list),
                Err(e) => {
                    tracing::warn!(
                        provider = %name,
                        endpoint_host = %haven_llm::endpoint_host(&base_url),
                        reason = e.connection_failure_reason().as_str(),
                        error = %haven_common::error::sanitize_error_text(&e.to_string()),
                        "discover_all_models failed"
                    );
                    (name.clone(), Vec::new())
                }
            }
        }));
    }
    for handle in handles {
        // A task panic yields an empty list for that provider.
        let (name, list) = match handle.await {
            Ok(entry) => entry,
            Err(e) => {
                tracing::warn!("discover_all_models task panicked: {}", e);
                continue;
            }
        };
        results.insert(name, list);
    }
    Ok(results)
}

/// Apply a model mutation through the runtime config coordinator. Command
/// validation and slot mutation remain here; the coordinator serializes the
/// durable edit and its complete live apply.
async fn update_model_field(
    state: &AppState,
    ctx: &str,
    model_id_or_request_kind: &str,
    validate: impl FnOnce(&haven_common::config::AppConfig, &str) -> Result<(), String>,
    mutate: impl FnOnce(&mut ModelConfig) -> Result<(), String>,
) -> Result<(), String> {
    state
        .config_apply_gate
        .edit_model_and_apply(state, ctx, |config| {
            validate(config, model_id_or_request_kind).map_err(anyhow::Error::msg)?;
            let slot = model_slot(&mut config.llm, model_id_or_request_kind).ok_or_else(|| {
                anyhow::anyhow!(
                    "unknown or unconfigured model/request: {}",
                    model_id_or_request_kind
                )
            })?;
            mutate(slot).map_err(anyhow::Error::msg)
        })
        .await
}

/// Select a configured model assignment as the primary for a request kind.
/// `role` is a RequestKind and `model_id` is a named ModelConfig id.
/// Updates config.toml and hot-swaps the LlmRouter at runtime.
#[tauri::command]
pub async fn switch_model(
    role: String,
    model_id: String,
    app: tauri::AppHandle,
) -> Result<(), String> {
    let state = app.state::<Arc<AppState>>();
    state
        .config_apply_gate
        .edit_model_and_apply(&state, "switch_model", |config| {
            set_request_route(&mut config.llm, &role, &model_id).map_err(anyhow::Error::msg)
        })
        .await?;
    crate::commands::emit_llm_config_changed(&app);
    Ok(())
}

/// Set the reasoning effort of a named model assignment (e.g. "low"/"medium"/"high").
/// Updates config.toml and hot-swaps the LlmRouter at runtime.
#[tauri::command]
pub async fn set_reasoning_effort(
    role: String,
    effort: Option<String>,
    app: tauri::AppHandle,
) -> Result<(), String> {
    let state = app.state::<Arc<AppState>>();

    let normalized = match effort {
        Some(e) if e.trim().is_empty() => None,
        Some(e) => Some(e.trim().to_string()),
        None => None,
    };

    update_model_field(
        &state,
        "set_reasoning_effort",
        &role,
        |_, _| Ok(()),
        |slot| {
            slot.reasoning_effort = normalized;
            Ok(())
        },
    )
    .await?;
    crate::commands::emit_llm_config_changed(&app);
    Ok(())
}

/// Set the provider built-in web search mode of a named model assignment
/// ("off" | "auto" | "always"). "auto" lets the model decide when to search;
/// any other value (including empty) is rejected. Updates config.toml and
/// hot-swaps the LlmRouter at runtime.
///
/// Non-`off` modes are rejected when the selected model/request's provider wire style does not
/// support a built-in search tool (see `supports_builtin_web_search`).
#[tauri::command]
pub async fn set_web_search(
    role: String,
    mode: Option<String>,
    app: tauri::AppHandle,
) -> Result<(), String> {
    let state = app.state::<Arc<AppState>>();

    let normalized = mode.as_deref().map(|m| m.trim().to_ascii_lowercase());
    match normalized.as_deref() {
        Some("off") | Some("auto") | Some("always") | None => {}
        _ => {
            return Err(log_err(
                "set_web_search",
                format!(
                    "invalid web search mode: {:?} (expected off|auto|always)",
                    mode
                ),
            ));
        }
    }
    let requires_builtin_search = !matches!(normalized.as_deref(), Some("off") | None);

    update_model_field(
        &state,
        "set_web_search",
        &role,
        |config, selector| {
            if !requires_builtin_search {
                return Ok(());
            }
            validate_builtin_search(config, selector)
        },
        |slot| {
            slot.web_search = normalized;
            Ok(())
        },
    )
    .await?;
    crate::commands::emit_llm_config_changed(&app);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use haven_common::config::{
        AppConfig, ModelConfig, ProviderConfig, RequestKind, RequestPolicy,
    };

    fn provider(name: &str, key: &str, style: Option<&str>) -> ProviderConfig {
        ProviderConfig {
            name: name.into(),
            api_key: key.into(),
            api_style: style.map(|s| s.into()),
            provider: style
                .filter(|s| *s == "llama.cpp")
                .unwrap_or("")
                .to_string(),
            ..Default::default()
        }
    }

    fn cfg_with_providers(providers: Vec<ProviderConfig>) -> AppConfig {
        let mut cfg = AppConfig::default();
        cfg.llm.providers = providers;
        cfg
    }

    #[test]
    fn explicit_discovery_key_uses_the_supplied_provider_auth_scheme() {
        let cfg = cfg_with_providers(Vec::new());

        let auth = resolve_discovery_auth(
            &cfg,
            "https://provider.example/v1",
            "entered-key",
            Some("not-yet-saved"),
            Some("x-api-key"),
            Some(""),
        );

        assert_eq!(
            auth,
            Some((
                "entered-key".into(),
                ("x-api-key".into(), "entered-key".into())
            ))
        );
    }

    #[test]
    fn stored_discovery_key_stays_bound_to_its_configured_endpoint() {
        let mut stored = provider("primary", "stored-key", Some("anthropic"));
        stored.base_url = "https://provider.example/v1".into();
        stored.auth_header_name = "x-api-key".into();
        stored.auth_header_prefix.clear();
        let cfg = cfg_with_providers(vec![stored]);

        let matching = resolve_discovery_auth(
            &cfg,
            "https://provider.example/v1",
            "",
            Some("primary"),
            None,
            None,
        );
        assert_eq!(
            matching,
            Some((
                "stored-key".into(),
                ("x-api-key".into(), "stored-key".into())
            ))
        );

        let mismatched = resolve_discovery_auth(
            &cfg,
            "https://other.example/v1",
            "",
            Some("primary"),
            Some("Authorization"),
            Some("Bearer"),
        );
        assert_eq!(mismatched, None);
    }

    #[test]
    fn api_key_status_reports_live_provider_configuration() {
        let cfg = cfg_with_providers(vec![
            provider("cloud", "sk-test", Some("openai-chat")),
            provider("empty", "", Some("openai-chat")),
            provider("local", "", Some("llama.cpp")),
        ]);
        let status = api_key_status(&cfg);
        assert!(status.providers["cloud"]);
        assert!(!status.providers["empty"]);
        assert!(status.providers["local"]);
        assert!(
            status
                .models
                .get("default_model")
                .is_none_or(|configured| !configured)
        );
    }

    #[test]
    fn api_key_status_has_a_named_wire_shape_without_credentials() {
        let mut cfg = AppConfig::default();
        cfg.media.ocr.api_key = "secret".into();
        let wire = serde_json::to_value(api_key_status(&cfg)).unwrap();
        assert_eq!(wire["ocr"], true);
        assert!(wire.get("api_key").is_none());
        assert!(wire["providers"].is_object());
    }

    #[test]
    fn provider_is_configured_accepts_key_or_llama_cpp() {
        assert!(provider_is_configured(&provider(
            "cloud",
            "sk",
            Some("openai-chat")
        )));
        assert!(!provider_is_configured(&provider(
            "cloud",
            "",
            Some("openai-chat")
        )));
        assert!(provider_is_configured(&provider(
            "local",
            "",
            Some("llama.cpp")
        )));
    }

    #[test]
    fn model_selector_accepts_named_model_id_or_request_kind() {
        let mut cfg = AppConfig::default();
        cfg.llm.models.push(ModelConfig {
            id: "chat-primary".into(),
            ..Default::default()
        });
        cfg.llm.request_policies.push(RequestPolicy {
            request: RequestKind::Chat,
            primary: "chat-primary".into(),
        });

        assert_eq!(
            model_id_for_selector(&cfg.llm, "chat-primary"),
            Some("chat-primary".into())
        );
        assert_eq!(
            model_id_for_selector(&cfg.llm, "chat"),
            Some("chat-primary".into())
        );
        assert_eq!(model_id_for_selector(&cfg.llm, "vision"), None);
    }

    #[test]
    fn switch_model_selects_a_named_model_config_for_the_request() {
        let mut llm =
            cfg_with_providers(vec![provider("primary", "api-key", Some("openai-chat"))]).llm;
        llm.models.extend([
            ModelConfig {
                id: "chat-primary".into(),
                provider: "primary".into(),
                model: "provider/model-a".into(),
                capabilities: vec![haven_common::config::Capability::Chat],
                ..Default::default()
            },
            ModelConfig {
                id: "chat-alternate".into(),
                provider: "primary".into(),
                model: "provider/model-b".into(),
                capabilities: vec![haven_common::config::Capability::Chat],
                ..Default::default()
            },
        ]);
        llm.set_policy(RequestKind::Chat, "chat-primary");

        set_request_route(&mut llm, "chat", "chat-alternate").unwrap();

        assert_eq!(
            llm.policy(RequestKind::Chat).unwrap().primary,
            "chat-alternate"
        );
        assert_eq!(
            llm.model("chat-alternate").unwrap().model,
            "provider/model-b"
        );
    }

    #[test]
    fn switch_model_rejects_unknown_or_incompatible_model_configs() {
        let mut llm =
            cfg_with_providers(vec![provider("primary", "api-key", Some("openai-chat"))]).llm;
        llm.models.push(ModelConfig {
            id: "embedding-only".into(),
            provider: "primary".into(),
            model: "provider/embedding".into(),
            capabilities: vec![haven_common::config::Capability::Embedding],
            ..Default::default()
        });
        llm.models.push(ModelConfig {
            id: "incomplete-chat".into(),
            capabilities: vec![haven_common::config::Capability::Chat],
            ..Default::default()
        });

        assert!(set_request_route(&mut llm, "chat", "missing").is_err());
        assert!(set_request_route(&mut llm, "chat", "embedding-only").is_err());
        assert!(set_request_route(&mut llm, "chat", "incomplete-chat").is_err());
        assert!(set_request_route(&mut llm, "invalid", "incomplete-chat").is_err());
        assert!(llm.policy(RequestKind::Chat).is_none());
    }

    #[test]
    fn web_search_validation_uses_selected_provider_capability_and_preserves_errors() {
        let mut cfg = cfg_with_providers(vec![provider(
            "chat-provider",
            "api-key",
            Some("openai-chat"),
        )]);
        cfg.llm.models.push(ModelConfig {
            id: "chat-model".into(),
            provider: "chat-provider".into(),
            ..Default::default()
        });
        cfg.llm.request_policies.push(RequestPolicy {
            request: RequestKind::Chat,
            primary: "chat-model".into(),
        });

        assert_eq!(
            validate_builtin_search(&cfg, "chat"),
            Err("provider wire style `openai-chat` does not support built-in search".into())
        );
        assert_eq!(
            validate_builtin_search(&cfg, "missing"),
            Err("unknown or unconfigured model/request: missing".into())
        );

        cfg.llm.providers[0].api_style = Some("openai-responses".into());
        assert_eq!(validate_builtin_search(&cfg, "chat-model"), Ok(()));
    }
}
