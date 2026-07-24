use serde::{Deserialize, Serialize};

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

    /// Fetch models from a provider's `/v1/models` endpoint
    pub async fn discover_from(
        &mut self,
        base_url: &str,
        api_key: &str,
    ) -> Result<Vec<ModelInfo>, crate::LlmError> {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .build()
            .map_err(|e| crate::LlmError::Unknown(e.to_string()))?;

        let url = format!("{}/models", base_url.trim_end_matches('/'));
        let resp = client
            .get(&url)
            .header("Authorization", format!("Bearer {}", api_key))
            .send()
            .await
            .map_err(crate::LlmError::from)?;

        if !resp.status().is_success() {
            return Ok(Vec::new());
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
                        Some(ModelInfo {
                            id,
                            provider: owned_by.to_string(),
                            name: m["id"].as_str().unwrap_or("").to_string(),
                            context_window: 128_000,
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
        self.builtin
            .iter()
            .chain(self.discovered.iter())
            .collect()
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
}