use serde::{Deserialize, Serialize};

use haven_common::config::ModelEndpoint;

/// Discovered (or statically listed) model metadata from a provider.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelInfo {
    pub id: String,
    pub provider: String,
    pub name: String,
    /// Input context window in tokens when the provider reported one; `0` = unknown.
    pub context_window: u32,
    pub supports_streaming: bool,
    pub supports_tools: bool,
    pub supports_vision: bool,
    /// USD per 1K input tokens when the provider reported pricing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_per_1k_input_tokens: Option<f64>,
    /// USD per 1K output tokens when the provider reported pricing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_per_1k_output_tokens: Option<f64>,
}

impl ModelInfo {
    /// Minimal entry used by STT-only static catalogs (no context / pricing).
    pub fn bare(id: impl Into<String>, provider: impl Into<String>) -> Self {
        let id = id.into();
        Self {
            name: id.clone(),
            id,
            provider: provider.into(),
            context_window: 0,
            supports_streaming: false,
            supports_tools: false,
            supports_vision: false,
            cost_per_1k_input_tokens: None,
            cost_per_1k_output_tokens: None,
        }
    }
}

/// Fallback window when neither the endpoint nor discovery supplied one.
pub const FALLBACK_CONTEXT_WINDOW: u32 = 128_000;

/// Resolve the effective context window for an endpoint.
///
/// Returns the explicit `context_window` when set and `> 0`. Otherwise `None`
/// — callers should fall back to `context_limits.default_context_window` (or
/// [`FALLBACK_CONTEXT_WINDOW`]). Builtin id catalogs are intentionally not
/// consulted; prefer provider `/models` metadata written into the role slot
/// when the user picks a model.
pub fn context_window_for(endpoint: &ModelEndpoint) -> Option<u32> {
    endpoint.context_window.filter(|w| *w > 0)
}

/// Parse a single OpenAI-compatible `/models` row into [`ModelInfo`].
///
/// Pulls whatever the gateway exposes: context length under several common
/// keys, display name, capability flags, and OpenRouter-style pricing.
pub fn model_info_from_json(m: &serde_json::Value) -> Option<ModelInfo> {
    let id = m.get("id").and_then(|v| v.as_str())?.to_string();
    if id.is_empty() {
        return None;
    }
    let name = m
        .get("name")
        .and_then(|v| v.as_str())
        .or_else(|| m.get("display_name").and_then(|v| v.as_str()))
        .unwrap_or(id.as_str())
        .to_string();
    let provider = m
        .get("owned_by")
        .and_then(|v| v.as_str())
        .or_else(|| m.get("provider").and_then(|v| v.as_str()))
        .unwrap_or("unknown")
        .to_string();
    let (cost_in, cost_out) = extract_pricing(m);
    Some(ModelInfo {
        id,
        provider,
        name,
        context_window: extract_context_window(m).unwrap_or(0),
        supports_streaming: m
            .get("supports_streaming")
            .and_then(|v| v.as_bool())
            .unwrap_or(true),
        supports_tools: extract_supports_tools(m),
        supports_vision: extract_supports_vision(m),
        cost_per_1k_input_tokens: cost_in,
        cost_per_1k_output_tokens: cost_out,
    })
}

fn json_u32(v: &serde_json::Value) -> Option<u32> {
    if let Some(n) = v.as_u64() {
        return u32::try_from(n).ok().filter(|n| *n > 0);
    }
    if let Some(n) = v.as_i64() {
        return u32::try_from(n).ok().filter(|n| *n > 0);
    }
    if let Some(n) = v.as_f64()
        && n.is_finite()
        && n > 0.0
        && n <= u32::MAX as f64
    {
        return Some(n as u32);
    }
    if let Some(s) = v.as_str() {
        let s = s.trim().replace('_', "");
        if let Ok(n) = s.parse::<u32>() {
            return (n > 0).then_some(n);
        }
        if let Ok(n) = s.parse::<f64>()
            && n.is_finite()
            && n > 0.0
            && n <= u32::MAX as f64
        {
            return Some(n as u32);
        }
    }
    None
}

fn extract_context_window(m: &serde_json::Value) -> Option<u32> {
    const KEYS: &[&str] = &[
        "context_length",
        "context_window",
        "max_model_len",
        "max_input_tokens",
        "max_sequence_length",
        "n_ctx",
    ];
    for key in KEYS {
        if let Some(v) = m.get(*key).and_then(json_u32) {
            return Some(v);
        }
    }
    const PATHS: &[&str] = &[
        "/top_provider/context_length",
        "/meta/n_ctx",
        "/meta/context_length",
        "/parameters/n_ctx",
        "/parameters/context_length",
        "/model_info/context_length",
        "/model_info/max_model_len",
    ];
    for path in PATHS {
        if let Some(v) = m.pointer(path).and_then(json_u32) {
            return Some(v);
        }
    }
    None
}

fn extract_supports_vision(m: &serde_json::Value) -> bool {
    if m.get("supports_vision").and_then(|v| v.as_bool()) == Some(true) {
        return true;
    }
    if m.pointer("/capabilities/vision").and_then(|v| v.as_bool()) == Some(true) {
        return true;
    }
    if let Some(mods) = m
        .pointer("/architecture/input_modalities")
        .and_then(|v| v.as_array())
        && mods.iter().any(|x| {
            matches!(
                x.as_str().map(|s| s.to_ascii_lowercase()).as_deref(),
                Some("image") | Some("vision")
            )
        })
    {
        return true;
    }
    if let Some(modality) = m
        .pointer("/architecture/modality")
        .and_then(|v| v.as_str())
        .map(|s| s.to_ascii_lowercase())
        && (modality.contains("image") || modality.contains("vision"))
    {
        return true;
    }
    false
}

fn extract_supports_tools(m: &serde_json::Value) -> bool {
    if let Some(b) = m.get("supports_tools").and_then(|v| v.as_bool()) {
        return b;
    }
    if m.pointer("/capabilities/tools").and_then(|v| v.as_bool()) == Some(true) {
        return true;
    }
    if let Some(params) = m.get("supported_parameters").and_then(|v| v.as_array())
        && params.iter().any(|x| {
            matches!(
                x.as_str().map(|s| s.to_ascii_lowercase()).as_deref(),
                Some("tools") | Some("tool_choice") | Some("functions")
            )
        })
    {
        return true;
    }
    // Most chat gateways support tools; leave true unless explicitly denied.
    true
}

/// OpenRouter / LiteLLM style: `pricing.prompt` / `pricing.completion` are
/// USD **per token**. Convert to USD per 1K tokens.
fn extract_pricing(m: &serde_json::Value) -> (Option<f64>, Option<f64>) {
    let prompt = m
        .pointer("/pricing/prompt")
        .or_else(|| m.pointer("/pricing/input"));
    let completion = m
        .pointer("/pricing/completion")
        .or_else(|| m.pointer("/pricing/output"));
    (
        prompt.and_then(per_token_to_per_1k),
        completion.and_then(per_token_to_per_1k),
    )
}

fn per_token_to_per_1k(v: &serde_json::Value) -> Option<f64> {
    let per_token = if let Some(n) = v.as_f64() {
        n
    } else {
        let s = v.as_str()?;
        s.trim().parse::<f64>().ok()?
    };
    if !per_token.is_finite() || per_token < 0.0 {
        return None;
    }
    Some(per_token * 1000.0)
}

pub struct ModelRegistry {
    discovered: Vec<ModelInfo>,
}

impl ModelRegistry {
    pub fn new() -> Self {
        Self {
            discovered: Vec::new(),
        }
    }

    /// Fetch models from a provider's `/models` endpoint.
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

        let base = base_url.trim_end_matches('/');
        let mut urls = vec![format!("{base}/models")];
        // Some OpenAI-compatible gateways are configured with their host root,
        // while exposing the OpenAI API below /v1 (for example ofox.ai and
        // PackyAPI). Retry that conventional path only when the first route is
        // unavailable, so providers that legitimately expose /models keep
        // their existing behavior.
        if !base.to_ascii_lowercase().ends_with("/v1") {
            urls.push(format!("{base}/v1/models"));
        }

        let mut resp = None;
        for (index, url) in urls.iter().enumerate() {
            let mut req = client.get(url);
            if let Some((name, value)) = auth_header {
                let name =
                    reqwest::header::HeaderName::from_bytes(name.as_bytes()).map_err(|_| {
                        crate::LlmError::InvalidResponse("invalid custom auth header name".into())
                    })?;
                let value = reqwest::header::HeaderValue::from_str(value).map_err(|_| {
                    crate::LlmError::InvalidResponse("invalid custom auth header value".into())
                })?;
                req = req.header(name, value);
            } else if !api_key.is_empty() {
                req = req.header("Authorization", format!("Bearer {}", api_key));
            }
            // OpenRouter ranks apps by these optional attribution headers.
            if base_url.to_ascii_lowercase().contains("openrouter") {
                req = req
                    .header("X-Title", "Haven")
                    .header("HTTP-Referer", "https://haven.app");
            }
            let candidate = req.send().await.map_err(crate::LlmError::from)?;
            let retry = index + 1 < urls.len() && matches!(candidate.status().as_u16(), 404 | 405);
            resp = Some(candidate);
            if !retry {
                break;
            }
        }
        let resp = resp.ok_or_else(|| {
            crate::LlmError::InvalidResponse("model discovery produced no response".into())
        })?;

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

        let models = parse_models_payload(&json);
        self.discovered = models;
        Ok(self.discovered.clone())
    }

    pub fn all(&self) -> Vec<&ModelInfo> {
        self.discovered.iter().collect()
    }

    pub fn search(&self, query: &str) -> Vec<&ModelInfo> {
        let q = query.to_lowercase();
        self.all()
            .into_iter()
            .filter(|m| m.id.to_lowercase().contains(&q) || m.name.to_lowercase().contains(&q))
            .collect()
    }
}

/// Soft cap on `/models` rows returned to the settings UI / IPC. Aggregators
/// like OpenRouter can return thousands of entries; keeping the full payload
/// freezes the settings page when every role picker remaps the list.
pub const DISCOVER_MODELS_CAP: usize = 500;

/// Accept OpenAI's `{ "data": [ ... ] }`, gateway wrappers such as
/// `{ "models": [ ... ] }`, or a bare `[ ... ]` array.
fn parse_models_payload(json: &serde_json::Value) -> Vec<ModelInfo> {
    let arr = json
        .get("data")
        .and_then(|v| v.as_array())
        .or_else(|| json.get("models").and_then(|v| v.as_array()))
        .or_else(|| json.get("result").and_then(|v| v.as_array()))
        .or_else(|| json.as_array());
    arr.map(|arr| {
        arr.iter()
            .filter_map(model_info_from_json)
            .take(DISCOVER_MODELS_CAP)
            .collect()
    })
    .unwrap_or_default()
}

impl Default for ModelRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

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
        assert_eq!(context_window_for(&endpoint), Some(1_000_000));
    }

    #[test]
    fn context_window_ignores_zero_explicit_config() {
        let mut endpoint = ep("unknown-model");
        endpoint.context_window = Some(0);
        assert_eq!(context_window_for(&endpoint), None);
    }

    #[test]
    fn context_window_none_without_explicit() {
        assert_eq!(context_window_for(&ep("gpt-4o")), None);
        assert_eq!(context_window_for(&ep("gemini-2.5-flash-001")), None);
    }

    #[test]
    fn parse_openai_minimal_row() {
        let m = model_info_from_json(&json!({
            "id": "gpt-4o",
            "owned_by": "openai"
        }))
        .unwrap();
        assert_eq!(m.id, "gpt-4o");
        assert_eq!(m.provider, "openai");
        assert_eq!(m.context_window, 0);
        assert!(m.supports_tools);
        assert!(!m.supports_vision);
    }

    #[test]
    fn parse_openrouter_enriched_row() {
        let m = model_info_from_json(&json!({
            "id": "openai/gpt-4o",
            "name": "GPT-4o",
            "context_length": 128000,
            "architecture": {
                "modality": "text+image",
                "input_modalities": ["text", "image"]
            },
            "supported_parameters": ["tools", "temperature"],
            "pricing": { "prompt": "0.0000025", "completion": "0.00001" }
        }))
        .unwrap();
        assert_eq!(m.context_window, 128_000);
        assert!(m.supports_vision);
        assert!(m.supports_tools);
        assert!((m.cost_per_1k_input_tokens.unwrap() - 0.0025).abs() < 1e-9);
        assert!((m.cost_per_1k_output_tokens.unwrap() - 0.01).abs() < 1e-9);
    }

    #[test]
    fn parse_vllm_max_model_len() {
        let m = model_info_from_json(&json!({
            "id": "local-llama",
            "max_model_len": 32768
        }))
        .unwrap();
        assert_eq!(m.context_window, 32_768);
    }

    #[test]
    fn parse_nested_meta_n_ctx() {
        let m = model_info_from_json(&json!({
            "id": "ollama-ish",
            "meta": { "n_ctx": "8192" }
        }))
        .unwrap();
        assert_eq!(m.context_window, 8192);
    }

    #[test]
    fn parse_models_payload_caps_large_catalogs() {
        let rows: Vec<_> = (0..DISCOVER_MODELS_CAP + 50)
            .map(|i| json!({ "id": format!("m-{i}") }))
            .collect();
        let models = parse_models_payload(&json!({ "data": rows }));
        assert_eq!(models.len(), DISCOVER_MODELS_CAP);
    }

    #[test]
    fn parse_models_payload_accepts_bare_array() {
        let models = parse_models_payload(&json!([
            { "id": "a", "context_length": 4096 },
            { "id": "b" }
        ]));
        assert_eq!(models.len(), 2);
        assert_eq!(models[0].context_window, 4096);
        assert_eq!(models[1].context_window, 0);
    }

    #[test]
    fn parse_models_payload_accepts_gateway_wrappers() {
        let models = parse_models_payload(&json!({
            "models": [{ "id": "packy-model" }]
        }));
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].id, "packy-model");
    }

    #[test]
    fn registry_starts_empty() {
        let reg = ModelRegistry::new();
        assert!(reg.all().is_empty());
        assert!(reg.search("gpt").is_empty());
    }
}
