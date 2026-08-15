use crate::app_state::AppState;
use crate::commands::log_err;
use crate::commands::rebuild_router;
use haven_common::config::{LlmConfig, ModelEndpoint};
use haven_llm::{EndpointRole, ModelInfo, ModelRegistry};
use std::sync::Arc;
use tauri::Manager;
use tauri::State;

/// Resolve a model role string to its endpoint slot, or `None` for unknown
/// roles. Single source of truth for the role names accepted by the model
/// commands (`switch_model`, `set_reasoning_effort`).
fn role_endpoint<'a>(cfg: &'a mut LlmConfig, role: &str) -> Option<&'a mut ModelEndpoint> {
    let endpoint = match EndpointRole::from_str(role)? {
        EndpointRole::SmallModel => &mut cfg.small_model,
        EndpointRole::DefaultModel => &mut cfg.default_model,
        EndpointRole::BalancedModel => &mut cfg.balanced_model,
        EndpointRole::ImageModel => &mut cfg.image_model,
        EndpointRole::AudioModel => &mut cfg.audio_model,
        EndpointRole::EmbeddingModel => &mut cfg.embedding_model,
    };
    Some(endpoint)
}

/// Normalize an endpoint URL for comparison: strip the trailing slash and
/// lowercase it (scheme/host comparisons are case-insensitive).
fn normalize_endpoint_url(url: &str) -> String {
    url.trim_end_matches('/').to_ascii_lowercase()
}

#[tauri::command]
pub async fn get_api_key_status() -> Result<serde_json::Value, String> {
    let loader =
        haven_common::config::ConfigLoader::load().map_err(|e| log_err("get_api_key_status", e))?;
    let cfg = loader.config();
    let mut status = serde_json::Map::new();
    for role in EndpointRole::ALL {
        let ep = match role {
            EndpointRole::SmallModel => &cfg.llm.small_model,
            EndpointRole::DefaultModel => &cfg.llm.default_model,
            EndpointRole::BalancedModel => &cfg.llm.balanced_model,
            EndpointRole::ImageModel => &cfg.llm.image_model,
            EndpointRole::AudioModel => &cfg.llm.audio_model,
            EndpointRole::EmbeddingModel => &cfg.llm.embedding_model,
        };
        status.insert(
            role.as_str().to_string(),
            serde_json::json!(!ep.api_key.is_empty()),
        );
    }
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
    let mut models_status = serde_json::Map::new();
    for m in &cfg.llm.models {
        models_status.insert(
            m.name.clone(),
            serde_json::json!(!m.endpoint.api_key.is_empty()),
        );
    }
    status.insert(
        "models".to_string(),
        serde_json::Value::Object(models_status),
    );
    Ok(serde_json::Value::Object(status))
}

/// Probe the configured default-model endpoint for live connectivity
/// (GET /models). The top-right status indicator uses this to show
/// Ready (green) when reachable or Disconnected (gray) when not.
#[tauri::command]
pub async fn check_llm_connection(state: State<'_, Arc<AppState>>) -> Result<bool, String> {
    Ok(state.agent.check_llm_connection().await)
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

/// §2.7: Fetch models from a provider's `/models` endpoint (OpenAI-
/// compatible). Used by the settings UI to populate the model dropdown after
/// the base URL and API key are entered. When `api_key` is empty (the
/// frontend masks stored keys) and `role` names a configured slot, the stored
/// key for that role is used — but only when the requested URL matches the
/// role's configured endpoint, so the stored key can never be sent to an
/// arbitrary renderer-supplied host.
#[tauri::command]
pub async fn discover_models(
    base_url: String,
    api_key: String,
    role: Option<String>,
    app: tauri::AppHandle,
) -> Result<Vec<ModelInfo>, String> {
    if !base_url.starts_with("http://") && !base_url.starts_with("https://") {
        return Err("base_url must be an http(s) URL".to_string());
    }
    let key = if api_key.is_empty() {
        if let Some(role) = role.as_deref() {
            let state = app.state::<Arc<AppState>>();
            let cfg = {
                let guard = state
                    .config_loader
                    .lock()
                    .map_err(|e| log_err("discover_models", e))?;
                guard.config().clone()
            };
            if role == "stt" {
                let stt = &cfg.media.stt;
                let stt_base = if stt.base_url.is_empty() {
                    stt_default_base_url(&stt.provider)
                } else {
                    stt.base_url.as_str()
                };
                if normalize_endpoint_url(stt_base) == normalize_endpoint_url(&base_url) {
                    stt.api_key.clone()
                } else {
                    String::new()
                }
            } else {
                let mut llm = cfg.llm.clone();
                match role_endpoint(&mut llm, role) {
                    Some(ep)
                        if normalize_endpoint_url(&ep.base_url)
                            == normalize_endpoint_url(&base_url) =>
                    {
                        ep.api_key.clone()
                    }
                    _ => String::new(),
                }
            }
        } else {
            String::new()
        }
    } else {
        api_key
    };
    // Resolve the role endpoint's auth scheme so discovery works for
    // Anthropic (`x-api-key`), Gemini (`x-goog-api-key`) and custom-gateway
    // endpoints — not just OpenAI-style `Authorization: Bearer`.
    let auth_header = {
        let state = app.state::<Arc<AppState>>();
        let cfg = {
            let guard = state
                .config_loader
                .lock()
                .map_err(|e| log_err("discover_models", e))?;
            guard.config().clone()
        };
        if role.as_deref() == Some("stt") {
            match cfg.media.stt.provider.as_str() {
                "gemini" => Some(("x-goog-api-key".to_string(), key.clone())),
                _ => Some(("Authorization".to_string(), format!("Bearer {}", key))),
            }
        } else {
            let mut llm = cfg.llm.clone();
            role.as_deref().and_then(|role| {
                role_endpoint(&mut llm, role)
                    .filter(|ep| {
                        normalize_endpoint_url(&ep.base_url) == normalize_endpoint_url(&base_url)
                    })
                    .map(|ep| {
                        let customized = ep.auth_header_name != "Authorization"
                            || ep.auth_header_prefix != "Bearer";
                        if customized {
                            (
                                ep.auth_header_name.clone(),
                                format!("{} {}", ep.auth_header_prefix, key),
                            )
                        } else {
                            match ep.provider.as_str() {
                                "anthropic" => ("x-api-key".to_string(), key.clone()),
                                "google" | "gemini" => ("x-goog-api-key".to_string(), key.clone()),
                                _ => ("Authorization".to_string(), format!("Bearer {}", key)),
                            }
                        }
                    })
            })
        }
    };
    let mut reg = ModelRegistry::new();
    tracing::info!("discovering models from {}", base_url);
    let models = reg
        .discover_from(
            &base_url,
            &key,
            auth_header.as_ref().map(|(n, v)| (n.as_str(), v.as_str())),
        )
        .await
        .map_err(|e| {
            tracing::warn!("model discovery failed for {}: {}", base_url, e);
            e.to_string()
        })?;
    tracing::info!("discovered {} models from {}", models.len(), base_url);
    Ok(models)
}

/// §2.7: Switch a model endpoint role to a different model.
/// Updates config.toml and hot-swaps the LlmRouter at runtime.
#[tauri::command]
/// Apply a mutation to a model endpoint via the shared config loader and
/// hot-swap the LlmRouter at runtime. Holds the loader lock across
/// mutate + save so concurrent config writes (settings saves, MCP/skill
/// toggles) can never clobber each other with a stale copy.
async fn update_endpoint_field(
    state: &AppState,
    ctx: &str,
    role: &str,
    mutate: impl FnOnce(&mut ModelEndpoint) -> Result<(), String>,
) -> Result<(), String> {
    {
        let mut loader = state.config_loader.lock().map_err(|e| log_err(ctx, e))?;
        let ep = role_endpoint(&mut loader.config_mut().llm, role)
            .ok_or_else(|| format!("unknown role: {}", role))?;
        mutate(ep)?;
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
    update_endpoint_field(&state, "switch_model", &role, |ep| {
        ep.model_name = model_id;
        Ok(())
    })
    .await
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

    update_endpoint_field(&state, "set_reasoning_effort", &role, |ep| {
        ep.reasoning_effort = normalized;
        Ok(())
    })
    .await
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

    update_endpoint_field(&state, "set_web_search", &role, |ep| {
        ep.web_search = normalized;
        Ok(())
    })
    .await
}
