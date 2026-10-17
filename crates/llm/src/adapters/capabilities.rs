//! Wire-protocol (`api_style`) capability helpers used by adapters.
//!
//! Canonical normalize / capability predicates live in
//! [`haven_common::config`] so model materialization can clear sticky
//! `web_search` without depending on this crate. This module imports those
//! predicates and owns [`WebSearchMode`] resolution shared by every adapter.
//!
//! | Stored / derived style     | Normalized wire   | Built-in web search |
//! |----------------------------|-------------------|---------------------|
//! | `openai-chat`              | `openai-chat`     | OpenAI search model / provider tools |
//! | `llama.cpp`                | `llama.cpp`       | no                  |
//! | `openai-responses`         | `openai-responses`| yes                 |
//! | `xai`                      | `xai`             | yes                 |
//! | `anthropic`                | `anthropic`       | yes                 |
//! | `gemini`                   | `gemini`          | yes                 |
//! | `deepgram` / `assemblyai`  | same              | n/a                 |

use haven_common::config::ModelEndpoint;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Web search mode for a provider's built-in search tool. Selected via the
/// endpoint's `web_search` config field or the `HAVEN_WEB_SEARCH` environment
/// variable (`off` | `auto` | `always`). Unconfigured defaults to `off`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WebSearchMode {
    /// Never expose / force the built-in search tool.
    Off,
    /// Let the model decide (`auto` / equivalent provider default).
    Auto,
    /// Force a search on every request when the protocol supports it.
    Always,
}

/// Parse a web search mode value (`off` | `auto` | `always`, case-insensitive).
/// Unset/empty/unrecognized values fall back to [`WebSearchMode::Off`].
fn parse_web_search_mode(value: Option<&str>) -> WebSearchMode {
    match value
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "auto" => WebSearchMode::Auto,
        "always" | "required" | "on" | "1" | "true" => WebSearchMode::Always,
        _ => WebSearchMode::Off,
    }
}

fn web_search_mode_from_env() -> WebSearchMode {
    parse_web_search_mode(std::env::var("HAVEN_WEB_SEARCH").ok().as_deref())
}

/// Resolve the effective web search mode for an endpoint: the endpoint's
/// `web_search` config field wins, then `HAVEN_WEB_SEARCH`, then `off`.
///
/// Callers should only inject tools when [`supports_builtin_web_search`] is
/// true for the endpoint's style; model materialization already clears
/// sticky values for unsupported styles.
pub fn resolve_web_search_mode(endpoint: &ModelEndpoint) -> WebSearchMode {
    match endpoint.web_search.as_deref() {
        Some(v) if !v.trim().is_empty() => parse_web_search_mode(Some(v)),
        _ => web_search_mode_from_env(),
    }
}

/// Dispatch the endpoint policy to the provider that owns its limits. Generic
/// wire styles retain their protocol-level behavior when no vendor policy
/// applies.
pub(crate) fn provider_builtin_web_search_available(
    endpoint: &ModelEndpoint,
    api_style: &str,
) -> bool {
    [
        super::openrouter::builtin_search_available(endpoint, api_style),
        super::dashscope::builtin_search_available(endpoint, api_style),
        super::openai::search_available(endpoint, api_style),
        super::groq::builtin_search_available(endpoint, api_style),
        super::deepseek::builtin_search_available(endpoint, api_style),
        super::xai::builtin_search_available(endpoint, api_style),
    ]
    .into_iter()
    .flatten()
    .next()
    .unwrap_or_else(|| {
        // OpenAI Responses defines the generic `web_search` tool. Chat
        // Completions has no protocol-wide search tool; vendor/model modules
        // must opt in above or the configured mode would be silently ignored.
        api_style == "openai-responses"
    })
}

/// Groq names its Responses API server tool `browser_search`; other current
/// Responses integrations use the OpenAI-compatible `web_search` name.
pub(crate) fn responses_web_search_tool_type(endpoint: &ModelEndpoint) -> &'static str {
    if let Some(tool_type) = super::openrouter::responses_search_tool_type(endpoint) {
        tool_type
    } else {
        super::groq::responses_search_tool_type(endpoint).unwrap_or("web_search")
    }
}

pub(crate) fn responses_search_can_force(endpoint: &ModelEndpoint) -> bool {
    [
        super::openrouter::responses_search_can_force(endpoint),
        super::dashscope::responses_search_can_force(endpoint),
    ]
    .into_iter()
    .flatten()
    .next()
    .unwrap_or(true)
}

/// Provider-specific fields that the OpenAI Chat Completions adapter merges
/// into its common request DTO. Each wire extension is built by its provider
/// module; this dispatcher only selects the applicable adapter.
#[derive(Default)]
pub(crate) struct ChatSearchRequest {
    pub(crate) server_tool_type: Option<&'static str>,
    pub(crate) search_parameters: Option<Value>,
    pub(crate) web_search_options: Option<Value>,
    pub(crate) enable_search: Option<bool>,
    pub(crate) include_tool_usage: Option<bool>,
    pub(crate) dashscope_search_options: Option<Value>,
    pub(crate) force_server_tool: bool,
}

pub(crate) fn chat_search_request(
    endpoint: &ModelEndpoint,
    api_style: &str,
    mode: WebSearchMode,
    has_function_tools: bool,
) -> ChatSearchRequest {
    let dashscope_enabled = super::dashscope::builtin_search_available(endpoint, api_style)
        == Some(true)
        && super::dashscope::chat_search_enabled(mode);

    ChatSearchRequest {
        server_tool_type: super::openrouter::chat_search_tool(endpoint, api_style, mode)
            .or_else(|| super::groq::chat_search_tool(endpoint, api_style, mode)),
        search_parameters: super::xai::chat_search_parameters(endpoint, api_style, mode),
        web_search_options: super::openai::search_options(endpoint, api_style, mode),
        enable_search: dashscope_enabled.then_some(true),
        include_tool_usage: dashscope_enabled.then_some(true),
        dashscope_search_options: dashscope_enabled
            .then(|| super::dashscope::chat_search_options(&endpoint.model_name, mode))
            .flatten(),
        force_server_tool: super::groq::chat_search_is_required(
            endpoint,
            api_style,
            mode,
            has_function_tools,
        ),
    }
}

/// Collect Chat Completions search activity using the owning provider's wire
/// schema. Adapters pass optional provider usage blocks through without
/// interpreting their fields here.
pub(crate) fn chat_search_calls(
    endpoint: &ModelEndpoint,
    api_style: &str,
    annotations: &[serde_json::Value],
    x_tools: Option<&serde_json::Value>,
    server_tool_use: Option<&serde_json::Value>,
) -> Vec<serde_json::Value> {
    [
        super::openai::search_call(endpoint, api_style, annotations),
        super::openrouter::chat_search_call(endpoint, api_style, annotations, server_tool_use),
        super::groq::chat_search_available(endpoint, api_style)
            .filter(|available| *available)
            .and_then(|_| super::groq::search_call(annotations)),
        super::dashscope::builtin_search_available(endpoint, api_style)
            .filter(|available| *available)
            .and_then(|_| super::dashscope::chat_search_call(x_tools)),
    ]
    .into_iter()
    .flatten()
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::xai_search_mode;
    use haven_common::config::{
        is_known_api_style, normalize_api_style, supports_builtin_web_search,
    };

    #[test]
    fn normalize_canonical_styles() {
        assert_eq!(normalize_api_style("OpenAI-Responses"), "openai-responses");
        assert_eq!(normalize_api_style("xai"), "xai");
        assert_eq!(normalize_api_style("unknown"), "invalid");
    }

    #[test]
    fn web_search_mode_uses_closed_snake_case_wire_values() {
        for (mode, wire) in [
            (WebSearchMode::Off, "\"off\""),
            (WebSearchMode::Auto, "\"auto\""),
            (WebSearchMode::Always, "\"always\""),
        ] {
            assert_eq!(serde_json::to_string(&mode).unwrap(), wire);
            assert_eq!(serde_json::from_str::<WebSearchMode>(wire).unwrap(), mode);
        }
        assert!(serde_json::from_str::<WebSearchMode>("\"required\"").is_err());
        assert!(serde_json::from_str::<WebSearchMode>("\"AUTO\"").is_err());
    }

    #[test]
    fn known_style_rejects_typos() {
        assert!(is_known_api_style("anthropic"));
        assert!(!is_known_api_style("deepseek-responses"));
        assert!(!is_known_api_style("grok"));
        assert!(!is_known_api_style("antropic"));
        assert!(!is_known_api_style("openai-respones"));
    }

    #[test]
    fn web_search_capability_matrix() {
        assert!(supports_builtin_web_search("openai-responses"));
        assert!(supports_builtin_web_search("openai-chat"));
        assert!(supports_builtin_web_search("xai"));
        assert!(supports_builtin_web_search("anthropic"));
        assert!(supports_builtin_web_search("gemini"));
        assert!(!supports_builtin_web_search("llama.cpp"));
        assert!(!supports_builtin_web_search("deepgram"));
        assert!(!supports_builtin_web_search("assemblyai"));
    }

    #[test]
    fn generic_chat_does_not_claim_an_unimplemented_search_tool() {
        assert!(!provider_builtin_web_search_available(
            &ModelEndpoint {
                provider: "custom-openai-compatible".into(),
                model_name: "some-model".into(),
                ..Default::default()
            },
            "openai-chat"
        ));
        assert!(provider_builtin_web_search_available(
            &ModelEndpoint {
                provider: "custom-responses".into(),
                model_name: "some-model".into(),
                ..Default::default()
            },
            "openai-responses"
        ));
    }

    #[test]
    fn provider_chat_search_request_fields_are_owned_by_vendor_adapters() {
        let openai = ModelEndpoint {
            provider: "openai".into(),
            model_name: "gpt-5-search-api".into(),
            ..Default::default()
        };
        assert_eq!(
            chat_search_request(&openai, "openai-chat", WebSearchMode::Auto, false)
                .web_search_options,
            Some(serde_json::json!({}))
        );

        let dashscope = ModelEndpoint {
            provider: "dashscope".into(),
            model_name: "qwen-plus".into(),
            ..Default::default()
        };
        let request = chat_search_request(&dashscope, "dashscope", WebSearchMode::Always, false);
        assert_eq!(request.enable_search, Some(true));
        assert_eq!(request.include_tool_usage, Some(true));
        assert_eq!(
            request.dashscope_search_options,
            Some(serde_json::json!({"forced_search": true}))
        );

        let groq = ModelEndpoint {
            provider: "groq".into(),
            model_name: "openai/gpt-oss-20b".into(),
            ..Default::default()
        };
        let request = chat_search_request(&groq, "openai-chat", WebSearchMode::Always, false);
        assert_eq!(request.server_tool_type, Some("browser_search"));
        assert!(request.force_server_tool);
        assert!(
            !chat_search_request(&groq, "openai-chat", WebSearchMode::Always, true)
                .force_server_tool
        );

        let openrouter = ModelEndpoint {
            provider: "openrouter".into(),
            model_name: "openai/gpt-5.5".into(),
            ..Default::default()
        };
        assert_eq!(
            chat_search_request(&openrouter, "openrouter", WebSearchMode::Auto, false)
                .server_tool_type,
            Some("openrouter:web_search")
        );
    }

    #[test]
    fn openrouter_and_dashscope_responses_leave_search_choice_to_model() {
        let openrouter = ModelEndpoint {
            provider: "openrouter".into(),
            ..Default::default()
        };
        let dashscope = ModelEndpoint {
            provider: "dashscope".into(),
            ..Default::default()
        };
        assert!(!responses_search_can_force(&openrouter));
        assert!(!responses_search_can_force(&dashscope));
        assert!(responses_search_can_force(&ModelEndpoint::default()));
    }

    #[test]
    fn parse_web_search_mode_maps_values() {
        assert_eq!(parse_web_search_mode(None), WebSearchMode::Off);
        assert_eq!(parse_web_search_mode(Some("")), WebSearchMode::Off);
        assert_eq!(parse_web_search_mode(Some("bogus")), WebSearchMode::Off);
        assert_eq!(parse_web_search_mode(Some("off")), WebSearchMode::Off);
        assert_eq!(parse_web_search_mode(Some("AUTO")), WebSearchMode::Auto);
        assert_eq!(parse_web_search_mode(Some("always")), WebSearchMode::Always);
        assert_eq!(parse_web_search_mode(Some("on")), WebSearchMode::Always);
    }

    #[test]
    fn xai_search_mode_mapping() {
        assert_eq!(xai_search_mode(WebSearchMode::Off), None);
        assert_eq!(xai_search_mode(WebSearchMode::Auto), Some("auto"));
        assert_eq!(xai_search_mode(WebSearchMode::Always), Some("on"));
    }

    #[test]
    fn resolve_prefers_endpoint_over_default() {
        let ep = ModelEndpoint {
            web_search: Some("auto".into()),
            ..Default::default()
        };
        assert_eq!(resolve_web_search_mode(&ep), WebSearchMode::Auto);
        let off = ModelEndpoint {
            web_search: None,
            ..Default::default()
        };
        assert_eq!(resolve_web_search_mode(&off), WebSearchMode::Off);
    }
}
