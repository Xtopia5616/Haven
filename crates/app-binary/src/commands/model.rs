use crate::app_state::AppState;
use crate::commands::log_err;
use crate::commands::rebuild_router;
use haven_common::config::{AppConfig, LlmConfig, ProviderConfig, RoleConfig};
use haven_llm::EndpointRole;
use haven_llm::ModelInfo;
use haven_llm::ModelRegistry;
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

/// Resolve the default base URL for an STT provider (used when the user has
/// not overridden it), so the stored-key guard in `discover_models` can match
/// the requested URL.
fn stt_default_base_url(provider: &str) -> &'static str {
    match provider {
        "groq" => "https://api.groq.com/openai/v1",
        "gemini" => "https://generativelanguage.googleapis.com/v1beta",
        "deepgram" => "https://api.deepgram.com/v1",
        "assemblyai" => "https://api.assemblyai.com",
        _ => "https://api.openai.com/v1",
    }
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

#[tauri::command]
pub async fn get_api_key_status() -> Result<serde_json::Value, String> {
    let loader =
        haven_common::config::ConfigLoader::load().map_err(|e| log_err("get_api_key_status", e))?;
    let cfg = loader.config();
    let mut status = serde_json::Map::new();
    // Per-role status: "usable" = references a configured provider + model.
    for role in EndpointRole::ALL {
        status.insert(
            role.as_str().to_string(),
            serde_json::json!(cfg.llm.is_configured(*role)),
        );
    }
    // Per-provider key status (the settings UI shows a StatusDot per provider).
    let mut providers_status = serde_json::Map::new();
    for p in &cfg.llm.providers {
        providers_status.insert(p.name.clone(), serde_json::json!(!p.api_key.is_empty()));
    }
    status.insert(
        "providers".to_string(),
        serde_json::Value::Object(providers_status),
    );
    status.insert(
        "stt".to_string(),
        serde_json::json!(!cfg.media.stt.api_key.is_empty()),
    );
    status.insert(
        "ocr".to_string(),
        serde_json::json!(!cfg.media.ocr.api_key.is_empty()),
    );
    status.insert(
        "tts".to_string(),
        serde_json::json!(!cfg.media.tts.api_key.is_empty()),
    );
    status.insert(
        "image_gen".to_string(),
        serde_json::json!(!cfg.media.image_gen.api_key.is_empty()),
    );
    Ok(serde_json::Value::Object(status))
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

/// §2.7: List available models from the built-in catalog
#[tauri::command]
pub async fn list_models(query: Option<String>) -> Result<Vec<ModelInfo>, String> {
    let reg = ModelRegistry::new();
    let results = match query {
        Some(q) if !q.is_empty() => {
            // Return owned ModelInfo from search
            let found = reg.search(&q);
            found.into_iter().cloned().collect()
        }
        _ => reg.all().into_iter().cloned().collect(),
    };
    Ok(results)
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
    let cfg = {
        let guard = state
            .config_loader
            .lock()
            .map_err(|e| log_err("discover_models", e))?;
        guard.config().clone()
    };

    let key_and_auth = if role.as_deref() == Some("stt") {
        // STT discovery: the key comes from `media.stt` (or the explicit one),
        // guarded by the STT provider's effective base URL.
        let stt = &cfg.media.stt;
        let stt_base = if stt.base_url.is_empty() {
            stt_default_base_url(&stt.provider)
        } else {
            stt.base_url.as_str()
        };
        let requested = normalize_endpoint_url(&base_url);
        if !api_key.is_empty() {
            let (h, pfx) = stt_auth_scheme(&stt.provider);
            let value = auth_value(&pfx, &api_key);
            Some((api_key.clone(), (h, value)))
        } else if normalize_endpoint_url(stt_base) == requested {
            let (h, pfx) = stt_auth_scheme(&stt.provider);
            let value = auth_value(&pfx, &stt.api_key);
            Some((stt.api_key.clone(), (h, value)))
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
pub async fn discover_all_models(app: tauri::AppHandle) -> Result<serde_json::Value, String> {
    let state = app.state::<Arc<AppState>>();
    let cfg = {
        let guard = state
            .config_loader
            .lock()
            .map_err(|e| log_err("discover_all_models", e))?;
        guard.config().clone()
    };
    let providers = cfg.llm.providers.clone();
    let mut handles = Vec::new();
    for p in &providers {
        if p.api_key.is_empty() || p.base_url.is_empty() {
            continue;
        }
        let (header, prefix) = provider_auth_scheme(p);
        let name = p.name.clone();
        let base_url = p.base_url.clone();
        let api_key = p.api_key.clone();
        let value = auth_value(&prefix, &api_key);
        handles.push(tokio::spawn(async move {
            let mut reg = ModelRegistry::new();
            match reg
                .discover_from(&base_url, &api_key, Some((header.as_str(), value.as_str())))
                .await
            {
                Ok(list) => (name.clone(), list),
                Err(e) => {
                    tracing::warn!("discover_all_models failed for {}: {}", name, e);
                    (name.clone(), Vec::new())
                }
            }
        }));
    }
    let mut results = std::collections::HashMap::new();
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
    serde_json::to_value(results).map_err(|e| log_err("discover_all_models", e))
}

/// §2.7: Switch a model endpoint role to a different model.
/// Updates config.toml and hot-swaps the LlmRouter at runtime.
#[tauri::command]
/// Apply a mutation to a role slot via the shared config loader and
/// hot-swap the LlmRouter at runtime. Holds the loader lock across
/// mutate + save so concurrent config writes (settings saves, MCP/skill
/// toggles) can never clobber each other with a stale copy.
async fn update_role_field(
    state: &AppState,
    ctx: &str,
    role: &str,
    mutate: impl FnOnce(&mut RoleConfig) -> Result<(), String>,
) -> Result<(), String> {
    {
        let mut loader = state.config_loader.lock().map_err(|e| log_err(ctx, e))?;
        let slot = role_slot(&mut loader.config_mut().llm, role)
            .ok_or_else(|| format!("unknown or unconfigured role: {}", role))?;
        mutate(slot)?;
        loader.save().map_err(|e| log_err(ctx, e))?;
    }
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

    update_role_field(&state, "set_web_search", &role, |slot| {
        slot.web_search = normalized;
        Ok(())
    })
    .await?;
    crate::commands::emit_llm_config_changed(&app);
    Ok(())
}
