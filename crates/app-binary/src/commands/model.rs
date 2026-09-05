use crate::app_state::AppState;
use crate::commands::log_err;
use crate::commands::rebuild_router;
use haven_common::config::{AppConfig, LlmConfig, ProviderConfig, RoleConfig};
use haven_llm::EndpointRole;
use haven_llm::ModelInfo;
use haven_llm::ModelRegistry;
use std::collections::BTreeMap;
use std::sync::Arc;
use tauri::Manager;
use tauri::State;

/// Resolve a model role string to its role slot (providers + roles world), or
/// `None` for unknown roles. Single source of truth for the role names
/// accepted by the model commands (`switch_model`, `set_reasoning_effort`,
/// `set_web_search`).
fn role_slot<'a>(cfg: &'a mut LlmConfig, role: &str) -> Option<&'a mut RoleConfig> {
    let role = EndpointRole::from_str(role)?;
    cfg.role_mut(role)
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
        stt_only_catalog(Some("deepgram"), "")
    } else if host.contains("assemblyai") {
        stt_only_catalog(Some("assemblyai"), "")
    } else {
        None
    }
}

/// Static model catalog for STT-only providers that have no `/models` list.
fn stt_only_catalog(api_style: Option<&str>, provider_hint: &str) -> Option<Vec<ModelInfo>> {
    let raw = api_style.unwrap_or(provider_hint);
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
        match p.api_style.as_deref() {
            Some("anthropic") => ("x-api-key".to_string(), String::new()),
            Some("gemini") => ("x-goog-api-key".to_string(), String::new()),
            _ => match p.provider.as_str() {
                "anthropic" => ("x-api-key".to_string(), String::new()),
                "google" | "gemini" => ("x-goog-api-key".to_string(), String::new()),
                _ => ("Authorization".to_string(), "Bearer".to_string()),
            },
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
/// - An explicit `api_key` wins; the auth scheme comes from the matching
///   configured provider (when the URL matches), falling back to OpenAI-style
///   `Authorization: Bearer`.
/// - An empty `api_key` falls back to the named provider's stored key, guarded
///   by URL: the provider is only used when its configured base URL matches
///   the requested one, so a stored key can never be sent to an arbitrary
///   renderer-supplied host.
fn resolve_discovery_auth(
    cfg: &AppConfig,
    base_url: &str,
    api_key: &str,
    provider: Option<&str>,
) -> Option<(String, (String, String))> {
    let requested = normalize_endpoint_url(base_url);
    let provider_cfg = provider.and_then(|name| cfg.llm.provider(name));

    if !api_key.is_empty() {
        let (h, pfx) = provider_cfg
            .filter(|p| normalize_endpoint_url(&p.base_url) == requested)
            .map(provider_auth_scheme)
            .unwrap_or_else(|| ("Authorization".to_string(), "Bearer".to_string()));
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

/// Build the `{role: bool, providers: {name: bool}, stt/ocr/...}` payload the
/// settings page uses for StatusDot / Set vs Change. Reads the live config
/// (same snapshot as model discovery), not a fresh disk reload.
/// TTS / image-gen reuse `providers` credentials, so they have no separate
/// key flags here.
#[derive(Debug, serde::Serialize)]
pub struct ApiKeyStatus {
    pub small_model: bool,
    pub default_model: bool,
    pub image_model: bool,
    pub audio_model: bool,
    pub embedding_model: bool,
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
    ApiKeyStatus {
        small_model: cfg.llm.is_configured(EndpointRole::SmallModel),
        default_model: cfg.llm.is_configured(EndpointRole::DefaultModel),
        image_model: cfg.llm.is_configured(EndpointRole::ImageModel),
        audio_model: cfg.llm.is_configured(EndpointRole::AudioModel),
        embedding_model: cfg.llm.is_configured(EndpointRole::EmbeddingModel),
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
/// Returns `"ready"` (reachable), `"disconnected"` (configured but
/// unreachable) or `"unconfigured"` (no api_key configured — no network
/// probe was attempted). The top-right status indicator maps these to
/// 就绪 / 已断开 / 未配置.
#[tauri::command]
pub async fn check_llm_connection(state: State<'_, Arc<AppState>>) -> Result<String, String> {
    Ok(state
        .agent
        .check_llm_connection()
        .await
        .as_str()
        .to_string())
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
/// an arbitrary renderer-supplied host. `role = "stt"` resolves through the
/// `media.stt` config instead (STT model discovery).
#[tauri::command]
pub async fn discover_models(
    base_url: String,
    api_key: String,
    provider: Option<String>,
    role: Option<String>,
    app: tauri::AppHandle,
) -> Result<Vec<ModelInfo>, String> {
    if !base_url.starts_with("http://") && !base_url.starts_with("https://") {
        return Err("base_url must be an http(s) URL".to_string());
    }
    let state = app.state::<Arc<AppState>>();
    let cfg = state
        .config_service
        .snapshot()
        .map_err(|e| log_err("discover_models", e))?
        .config;

    // STT-only providers (Deepgram / AssemblyAI) have no `/models` endpoint;
    // return the static catalog so the role picker can still assign a model.
    // The selected STT value is always a name from `llm.providers`; unsaved
    // discovery can still use the requested URL host below.
    if let Some(name) = provider.as_deref().filter(|n| !n.is_empty())
        && let Some(p) = cfg.llm.provider(name)
        && let Some(list) = stt_only_catalog(p.api_style.as_deref(), p.provider.as_str())
    {
        return Ok(list);
    }
    if let Some(list) = stt_only_catalog_for_url(&base_url) {
        return Ok(list);
    }

    let key_and_auth = if role.as_deref() == Some("stt") {
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
            Some((api_key.clone(), (h, value)))
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
            Some((p.api_key.clone(), (h, value)))
        } else {
            None
        }
    } else {
        resolve_discovery_auth(&cfg, &base_url, &api_key, provider.as_deref())
    };

    let (key, (header, value)) = key_and_auth.ok_or_else(|| {
        "未找到可用的 API Key：请填写 API Key，或先保存 Provider 配置（其 Base URL 需与请求地址一致）"
            .to_string()
    })?;

    let mut reg = ModelRegistry::new();
    tracing::info!("discovering models from {}", base_url);
    let models = reg
        .discover_from(&base_url, &key, Some((header.as_str(), value.as_str())))
        .await
        .map_err(|e| {
            tracing::warn!("model discovery failed for {}: {}", base_url, e);
            e.to_string()
        })?;
    tracing::info!("discovered {} models from {}", models.len(), base_url);
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
        if let Some(list) = stt_only_catalog(p.api_style.as_deref(), p.provider.as_str()) {
            results.insert(p.name.clone(), list);
            continue;
        }
        let (header, prefix) = provider_auth_scheme(p);
        let name = p.name.clone();
        let base_url = p.base_url.clone();
        let api_key = p.api_key.clone();
        let auth_header = if api_key.is_empty() {
            None
        } else {
            Some((header, auth_value(&prefix, &api_key)))
        };
        handles.push(tokio::spawn(async move {
            let mut reg = ModelRegistry::new();
            let auth_ref = auth_header.as_ref().map(|(h, v)| (h.as_str(), v.as_str()));
            match reg.discover_from(&base_url, &api_key, auth_ref).await {
                Ok(list) => (name.clone(), list),
                Err(e) => {
                    tracing::warn!("discover_all_models failed for {}: {}", name, e);
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

/// §2.7: Switch a model endpoint role to a different model.
/// Updates config.toml and hot-swaps the LlmRouter at runtime.
#[tauri::command]
/// Apply a mutation to a role slot through the versioned config service and
/// hot-swap the LlmRouter at runtime. The service serializes the mutation and
/// persists the complete snapshot before the runtime rebuild begins.
async fn update_role_field(
    state: &AppState,
    ctx: &str,
    role: &str,
    mutate: impl FnOnce(&mut RoleConfig) -> Result<(), String>,
) -> Result<(), String> {
    state
        .config_service
        .edit(|config| {
            let slot = role_slot(&mut config.llm, role)
                .ok_or_else(|| anyhow::anyhow!("unknown or unconfigured role: {}", role))?;
            mutate(slot).map_err(anyhow::Error::msg)
        })
        .map_err(|e| log_err(ctx, e))?;
    rebuild_router(state, ctx).await
}

/// Switch a model endpoint role to another model id. Updates config.toml and
/// hot-swaps the LlmRouter at runtime.
#[tauri::command]
pub async fn switch_model(
    role: String,
    model_id: String,
    app: tauri::AppHandle,
) -> Result<(), String> {
    let state = app.state::<Arc<AppState>>();
    update_role_field(&state, "switch_model", &role, |slot| {
        slot.model = model_id;
        Ok(())
    })
    .await?;
    crate::commands::emit_llm_config_changed(&app);
    Ok(())
}

/// Set the reasoning effort of a model endpoint role (e.g. "low"/"medium"/"high").
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

    update_role_field(&state, "set_reasoning_effort", &role, |slot| {
        slot.reasoning_effort = normalized;
        Ok(())
    })
    .await?;
    crate::commands::emit_llm_config_changed(&app);
    Ok(())
}

/// Set the provider built-in web search mode of a model endpoint role
/// ("off" | "auto" | "always"). "auto" lets the model decide when to search;
/// any other value (including empty) is rejected. Updates config.toml and
/// hot-swaps the LlmRouter at runtime.
///
/// Non-`off` modes are rejected when the role's provider wire style does not
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
            return Err(format!(
                "invalid web search mode: {:?} (expected off|auto|always)",
                mode
            ));
        }
    }

    // Capability gate: only `off` (or clear) is allowed on styles without a
    // provider built-in search tool. Resolve style from one immutable snapshot
    // before `update_role_field` applies the typed mutation.
    if !matches!(normalized.as_deref(), Some("off") | None) {
        let style = {
            let loader = state
                .config_service
                .snapshot()
                .map_err(|e| log_err("set_web_search", e))?;
            let llm = &loader.config.llm;
            let slot = llm
                .roles
                .iter()
                .find(|r| r.role == role)
                .ok_or_else(|| format!("unknown or unconfigured role: {}", role))?;
            llm.providers
                .iter()
                .find(|p| p.name == slot.provider)
                .and_then(|p| {
                    p.api_style
                        .as_deref()
                        .filter(|s| !s.is_empty())
                        .map(str::to_string)
                        .or_else(|| {
                            if p.provider.is_empty() {
                                None
                            } else {
                                Some(p.provider.clone())
                            }
                        })
                })
                .unwrap_or_else(|| "openai-chat".into())
        };
        if !haven_llm::supports_builtin_web_search(&style) {
            return Err(format!(
                "provider wire style `{style}` does not support built-in web search"
            ));
        }
    }

    update_role_field(&state, "set_web_search", &role, |slot| {
        slot.web_search = normalized;
        Ok(())
    })
    .await?;
    crate::commands::emit_llm_config_changed(&app);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use haven_common::config::{AppConfig, ProviderConfig};

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
        assert!(!status.default_model);
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
}
