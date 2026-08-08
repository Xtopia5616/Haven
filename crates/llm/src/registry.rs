use serde::{Deserialize, Serialize};

use haven_common::config::ModelEndpoint;

/// §2.7: Model registry for automatic model discovery
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelInfo {
    pub id: String,
    pub provider: String,
    pub name: String,
    pub context_window: u32,
    pub supports_streaming: bool,
    pub supports_tools: bool,
    pub supports_vision: bool,
}

/// Built-in catalog of known models
pub fn builtin_catalog() -> Vec<ModelInfo> {
    vec![
        ModelInfo {
            id: "gpt-4o".into(),
            provider: "openai".into(),
            name: "GPT-4o".into(),
            context_window: 128_000,
            supports_streaming: true,
            supports_tools: true,
            supports_vision: true,
        },
        ModelInfo {
            id: "gpt-4o-mini".into(),
            provider: "openai".into(),
            name: "GPT-4o Mini".into(),
            context_window: 128_000,
            supports_streaming: true,
            supports_tools: true,
            supports_vision: true,
        },
        ModelInfo {
            id: "gpt-4.1-nano".into(),
            provider: "openai".into(),
            name: "GPT-4.1 Nano".into(),
            context_window: 1_000_000,
            supports_streaming: true,
            supports_tools: true,
            supports_vision: true,
        },
        ModelInfo {
            id: "claude-sonnet-4-20250514".into(),
            provider: "anthropic".into(),
            name: "Claude Sonnet 4".into(),
            context_window: 200_000,
            supports_streaming: true,
            supports_tools: true,
            supports_vision: true,
        },
        ModelInfo {
            id: "gemini-2.5-flash-001".into(),
            provider: "google".into(),
            name: "Gemini 2.5 Flash".into(),
            context_window: 1_000_000,
            supports_streaming: true,
            supports_tools: true,
            supports_vision: true,
        },
        ModelInfo {
            id: "deepseek-chat".into(),
            provider: "deepseek".into(),
            name: "DeepSeek Chat".into(),
            context_window: 64_000,
            supports_streaming: true,
            supports_tools: true,
            supports_vision: false,
        },
    ]
}

/// Resolve the effective context window (tokens) for an endpoint.
///
/// Priority:
/// 1. Explicit `context_window` set in the endpoint config.
/// 2. Builtin catalog match on `model_name` (provider-prefixed ids like
///    `openai/gpt-4.1-nano` are normalized, and date-suffixed ids like
///    `gpt-4o-2024-08-06` match the base catalog entry).
/// 3. A 128K default when the model is unknown.
///
/// This is the value that drives context compaction and the token-usage
/// display, so it must reflect the model's real input budget — not the
/// per-response output cap (`max_tokens`).
pub fn context_window_for(endpoint: &ModelEndpoint) -> u32 {
    if let Some(window) = endpoint.context_window.filter(|w| *w > 0) {
        return window;
    }
    let model = endpoint.model_name.trim();
    let model = model
        .rsplit_once('/')
        .map(|(_, id)| id)
        .unwrap_or(model)
        .trim()
        .to_ascii_lowercase();
    let catalog = builtin_catalog();
    // 1. Exact id/name match.
    if let Some(info) = catalog
        .iter()
        .find(|m| m.id.to_ascii_lowercase() == model || m.name.to_ascii_lowercase() == model)
    {
        return info.context_window;
    }
    // 2. The configured id starts with a catalog id (e.g. dated model
    //    revisions like `gpt-4o-2024-08-06` or `claude-sonnet-4-20250514`).
    if let Some(info) = catalog.iter().find(|m| {
        let id = m.id.to_ascii_lowercase();
        !model.is_empty() && id.len() < model.len() && model.starts_with(&id)
    }) {
        return info.context_window;
    }
    128_000
}

pub struct ModelRegistry {
    builtin: Vec<ModelInfo>,
    discovered: Vec<ModelInfo>,
}

impl ModelRegistry {
    pub fn new() -> Self {
        Self {
            builtin: builtin_catalog(),
            discovered: Vec::new(),
        }
    }

    /// Fetch models from a provider's `/v1/models` endpoint.
    ///
    /// `auth_header` overrides the default `Authorization: Bearer <key>`
    /// scheme with a literal `(header_name, header_value)` pair — used by the
    /// settings UI for Anthropic (`x-api-key`), Gemini (`x-goog-api-key`) and
    /// custom-gateway endpoints.
    pub async fn discover_from(
        &mut self,
        base_url: &str,
        api_key: &str,
        auth_header: Option<(&str, &str)>,
    ) -> Result<Vec<ModelInfo>, crate::LlmError> {
        let client = crate::client::http_client_builder()
            .timeout(std::time::Duration::from_secs(10))
            .build()
            .map_err(|e| crate::LlmError::Unknown(e.to_string()))?;

        let url = format!("{}/models", base_url.trim_end_matches('/'));
        let mut req = client.get(&url);
        if let Some((name, value)) = auth_header {
            if let Ok(name) = reqwest::header::HeaderName::from_bytes(name.as_bytes())
                && let Ok(value) = reqwest::header::HeaderValue::from_str(value)
            {
                req = req.header(name, value);
            }
        } else if !api_key.is_empty() {
            req = req.header("Authorization", format!("Bearer {}", api_key));
        }
        let resp = req.send().await.map_err(crate::LlmError::from)?;

        if !resp.status().is_success() {
            let code = resp.status().as_u16();
            return Err(if code == 401 || code == 403 {
                crate::LlmError::Auth(format!("status {}", resp.status()))
            } else {
                crate::LlmError::ServerError(format!("status {}", resp.status()))
            });
        }

        let json: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| crate::LlmError::InvalidResponse(e.to_string()))?;

        let models: Vec<ModelInfo> = json["data"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(|m| {
                        let id = m["id"].as_str()?.to_string();
                        let owned_by = m["owned_by"].as_str().unwrap_or("unknown");
                        let window = context_window_for(&ModelEndpoint {
                            model_name: id.clone(),
                            ..Default::default()
                        });
                        Some(ModelInfo {
                            id,
                            provider: owned_by.to_string(),
                            name: m["id"].as_str().unwrap_or("").to_string(),
                            context_window: window,
                            supports_streaming: true,
                            supports_tools: true,
                            supports_vision: false,
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();

        self.discovered = models.clone();
        Ok(models)
    }

    pub fn all(&self) -> Vec<&ModelInfo> {
        self.builtin.iter().chain(self.discovered.iter()).collect()
    }

    pub fn search(&self, query: &str) -> Vec<&ModelInfo> {
        let q = query.to_lowercase();
        self.all()
            .into_iter()
            .filter(|m| m.id.to_lowercase().contains(&q) || m.name.to_lowercase().contains(&q))
            .collect()
    }
}

impl Default for ModelRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_catalog_has_known_models() {
        let cat = builtin_catalog();
        assert!(!cat.is_empty());
        assert!(cat.iter().any(|m| m.id.contains("gpt-4o")));
    }

    #[test]
    fn registry_has_builtins() {
        let reg = ModelRegistry::new();
        assert!(!reg.all().is_empty());
    }

    #[test]
    fn search_filters_by_name() {
        let reg = ModelRegistry::new();
        let results = reg.search("claude");
        assert!(results.iter().any(|m| m.id.contains("claude")));
    }

    fn ep(model_name: &str) -> ModelEndpoint {
        ModelEndpoint {
            model_name: model_name.into(),
            ..Default::default()
        }
    }

    #[test]
    fn context_window_prefers_explicit_config() {
        let mut endpoint = ep("unknown-model");
        endpoint.context_window = Some(1_000_000);
        assert_eq!(context_window_for(&endpoint), 1_000_000);
    }

    #[test]
    fn context_window_ignores_zero_explicit_config() {
        let mut endpoint = ep("unknown-model");
        endpoint.context_window = Some(0);
        assert_eq!(context_window_for(&endpoint), 128_000);
    }

    #[test]
    fn context_window_matches_builtin_by_id() {
        assert_eq!(context_window_for(&ep("gemini-2.5-flash-001")), 1_000_000);
        assert_eq!(context_window_for(&ep("deepseek-chat")), 64_000);
    }

    #[test]
    fn context_window_matches_provider_prefixed_id() {
        assert_eq!(context_window_for(&ep("openai/gpt-4.1-nano")), 1_000_000);
    }

    #[test]
    fn context_window_matches_dated_revision_id() {
        // gpt-4o-2024-08-06 starts with catalog id "gpt-4o" (128K).
        assert_eq!(context_window_for(&ep("gpt-4o-2024-08-06")), 128_000);
    }

    #[test]
    fn context_window_defaults_when_unknown() {
        assert_eq!(context_window_for(&ep("my-custom-llm")), 128_000);
    }
}
