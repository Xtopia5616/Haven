use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::time::Duration;

pub(crate) const MAX_FEATURE_LIST_PAGES: usize = 100;
pub(crate) const MAX_FEATURE_LIST_ITEMS: usize = 10_000;
pub(crate) const MAX_FEATURE_LIST_BYTES: usize = 4 * 1024 * 1024;
pub(crate) const MAX_FEATURE_CURSOR_BYTES: usize = 4096;
pub(crate) const MAX_RESOURCE_CONTENTS: usize = 64;
pub(crate) const MAX_RESOURCE_TEXT_BYTES: usize = 1024 * 1024;
pub(crate) const MAX_RESOURCE_BLOB_BYTES: usize = 2 * 1024 * 1024;
pub(crate) const MAX_RESOURCE_RESPONSE_BYTES: usize = 3 * 1024 * 1024;
pub(crate) const MAX_RESOURCE_SUBSCRIPTIONS: usize = 64;
pub(crate) const MAX_PROMPT_MESSAGES: usize = 128;
pub(crate) const MAX_PROMPT_CONTENT_BYTES: usize = 512 * 1024;
pub(crate) const MAX_PROMPT_ARGUMENTS: usize = 128;
pub(crate) const MAX_PROMPT_ARGUMENT_BYTES: usize = 16 * 1024;
pub(crate) const MAX_FEATURE_CACHE_TTL: Duration = Duration::from_secs(24 * 60 * 60);
pub(crate) const MAX_FEATURE_NAME_BYTES: usize = 1024;
pub(crate) const MAX_FEATURE_DESCRIPTION_BYTES: usize = 16 * 1024;
pub(crate) const MAX_FEATURE_URI_BYTES: usize = 8192;
pub(crate) const MAX_FEATURE_MIME_BYTES: usize = 512;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum McpCacheScope {
    Public,
    Private,
}

impl McpCacheScope {
    pub(crate) fn from_hint(value: Option<&str>) -> Self {
        if value == Some("public") {
            Self::Public
        } else {
            Self::Private
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct McpServerCapabilities {
    pub(crate) tools: bool,
    pub(crate) resources: bool,
    pub(crate) resource_subscribe: bool,
    pub(crate) prompts: bool,
}

impl McpServerCapabilities {
    pub(crate) fn from_wire_capabilities(value: Option<&Value>) -> Self {
        let resources = value.and_then(|caps| caps.get("resources"));
        Self {
            tools: value.and_then(|caps| caps.get("tools")).is_some(),
            resources: resources.is_some_and(Value::is_object),
            resource_subscribe: resources
                .and_then(|caps| caps.get("subscribe"))
                .and_then(Value::as_bool)
                .unwrap_or(false),
            prompts: value
                .and_then(|caps| caps.get("prompts"))
                .is_some_and(Value::is_object),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpResourceInfo {
    pub uri: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct McpResourceList {
    pub resources: Vec<McpResourceInfo>,
    pub ttl_ms: Option<u64>,
    pub cache_scope: McpCacheScope,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpResourceTemplate {
    pub uri_template: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct McpResourceTemplateList {
    pub resource_templates: Vec<McpResourceTemplate>,
    pub ttl_ms: Option<u64>,
    pub cache_scope: McpCacheScope,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpResourceContent {
    pub uri: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blob: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct McpResourceRead {
    pub contents: Vec<McpResourceContent>,
    pub ttl_ms: Option<u64>,
    pub cache_scope: McpCacheScope,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpPromptArgument {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default)]
    pub required: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpPromptInfo {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default)]
    pub arguments: Vec<McpPromptArgument>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct McpPromptList {
    pub prompts: Vec<McpPromptInfo>,
    pub ttl_ms: Option<u64>,
    pub cache_scope: McpCacheScope,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpPromptMessage {
    pub role: String,
    pub content: McpPromptContent,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum McpPromptContent {
    Text {
        text: String,
    },
    Image {
        data: String,
        mime_type: String,
    },
    Audio {
        data: String,
        mime_type: String,
    },
    ResourceLink {
        uri: String,
        name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        title: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        description: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mime_type: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        size: Option<u64>,
    },
    Resource {
        resource: McpResourceContent,
    },
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct McpPrompt {
    pub description: Option<String>,
    pub messages: Vec<McpPromptMessage>,
    pub ttl_ms: Option<u64>,
    pub cache_scope: McpCacheScope,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct McpResourceListPage {
    pub(crate) resources: Vec<McpResourceInfo>,
    #[serde(default)]
    pub(crate) next_cursor: Option<String>,
    #[serde(default)]
    pub(crate) ttl_ms: Option<u64>,
    #[serde(default)]
    pub(crate) cache_scope: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct McpResourceTemplateListPage {
    pub(crate) resource_templates: Vec<McpResourceTemplate>,
    #[serde(default)]
    pub(crate) next_cursor: Option<String>,
    #[serde(default)]
    pub(crate) ttl_ms: Option<u64>,
    #[serde(default)]
    pub(crate) cache_scope: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct McpPromptListPage {
    pub(crate) prompts: Vec<McpPromptInfo>,
    #[serde(default)]
    pub(crate) next_cursor: Option<String>,
    #[serde(default)]
    pub(crate) ttl_ms: Option<u64>,
    #[serde(default)]
    pub(crate) cache_scope: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct McpResourceReadResponse {
    pub(crate) contents: Vec<McpResourceContent>,
    #[serde(default)]
    pub(crate) ttl_ms: Option<u64>,
    #[serde(default)]
    pub(crate) cache_scope: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct McpPromptResponse {
    #[serde(default)]
    pub(crate) description: Option<String>,
    pub(crate) messages: Vec<McpPromptMessage>,
}

pub(crate) fn validate_complete_result(result: &Value, method: &str) -> Result<(), String> {
    match result.get("resultType") {
        Some(Value::String(result_type)) if result_type == "complete" => Ok(()),
        Some(Value::String(result_type)) if result_type == "input_required" => Err(format!(
            "MCP {method} requires additional interaction; Haven did not execute it"
        )),
        Some(_) => Err(format!("MCP {method} returned an unsupported result type")),
        None => Err(format!(
            "MCP {method} response omitted the required result type"
        )),
    }
}

pub(crate) fn validate_resource_info(resource: &McpResourceInfo) -> Result<(), String> {
    validate_identity("resource URI", &resource.uri, MAX_FEATURE_URI_BYTES)?;
    validate_identity("resource name", &resource.name, MAX_FEATURE_NAME_BYTES)?;
    validate_optional_text(
        "resource title",
        resource.title.as_deref(),
        MAX_FEATURE_NAME_BYTES,
    )?;
    validate_optional_text(
        "resource description",
        resource.description.as_deref(),
        MAX_FEATURE_DESCRIPTION_BYTES,
    )?;
    validate_optional_text(
        "resource MIME type",
        resource.mime_type.as_deref(),
        MAX_FEATURE_MIME_BYTES,
    )?;
    Ok(())
}

pub(crate) fn validate_resource_template(template: &McpResourceTemplate) -> Result<(), String> {
    validate_identity(
        "resource URI template",
        &template.uri_template,
        MAX_FEATURE_URI_BYTES,
    )?;
    validate_identity(
        "resource template name",
        &template.name,
        MAX_FEATURE_NAME_BYTES,
    )?;
    validate_optional_text(
        "resource template title",
        template.title.as_deref(),
        MAX_FEATURE_NAME_BYTES,
    )?;
    validate_optional_text(
        "resource template description",
        template.description.as_deref(),
        MAX_FEATURE_DESCRIPTION_BYTES,
    )?;
    validate_optional_text(
        "resource template MIME type",
        template.mime_type.as_deref(),
        MAX_FEATURE_MIME_BYTES,
    )?;
    Ok(())
}

pub(crate) fn validate_prompt_info(prompt: &McpPromptInfo) -> Result<(), String> {
    validate_identity("prompt name", &prompt.name, MAX_FEATURE_NAME_BYTES)?;
    validate_optional_text(
        "prompt title",
        prompt.title.as_deref(),
        MAX_FEATURE_NAME_BYTES,
    )?;
    validate_optional_text(
        "prompt description",
        prompt.description.as_deref(),
        MAX_FEATURE_DESCRIPTION_BYTES,
    )?;
    if prompt.arguments.len() > MAX_PROMPT_ARGUMENTS {
        return Err("MCP prompt exceeded the argument limit".into());
    }
    let mut names = std::collections::HashSet::new();
    for argument in &prompt.arguments {
        validate_identity(
            "prompt argument name",
            &argument.name,
            MAX_FEATURE_NAME_BYTES,
        )?;
        if !names.insert(argument.name.as_str()) {
            return Err("MCP prompt returned duplicate argument names".into());
        }
        validate_optional_text(
            "prompt argument title",
            argument.title.as_deref(),
            MAX_FEATURE_NAME_BYTES,
        )?;
        validate_optional_text(
            "prompt argument description",
            argument.description.as_deref(),
            MAX_FEATURE_DESCRIPTION_BYTES,
        )?;
    }
    Ok(())
}

pub(crate) fn validate_resource_contents(contents: &[McpResourceContent]) -> Result<usize, String> {
    if contents.len() > MAX_RESOURCE_CONTENTS {
        return Err("MCP resources/read exceeded the content limit".into());
    }
    let mut aggregate_bytes = 0usize;
    for content in contents {
        validate_identity("resource URI", &content.uri, MAX_FEATURE_URI_BYTES)?;
        validate_optional_text(
            "resource MIME type",
            content.mime_type.as_deref(),
            MAX_FEATURE_MIME_BYTES,
        )?;
        match (&content.text, &content.blob) {
            (Some(text), None) => {
                if text.len() > MAX_RESOURCE_TEXT_BYTES {
                    return Err("MCP resource text exceeded the byte limit".into());
                }
                aggregate_bytes = aggregate_bytes.saturating_add(text.len());
            }
            (None, Some(blob)) => {
                let decoded = base64::engine::general_purpose::STANDARD
                    .decode(blob)
                    .map_err(|_| "MCP resource returned invalid Base64 content".to_string())?;
                if decoded.len() > MAX_RESOURCE_BLOB_BYTES {
                    return Err("MCP resource blob exceeded the byte limit".into());
                }
                aggregate_bytes = aggregate_bytes.saturating_add(decoded.len());
            }
            _ => {
                return Err(
                    "MCP resource content must contain exactly one text or blob field".into(),
                );
            }
        }
        if aggregate_bytes > MAX_RESOURCE_RESPONSE_BYTES {
            return Err("MCP resources/read exceeded the aggregate byte limit".into());
        }
    }
    Ok(aggregate_bytes)
}

pub(crate) fn validate_prompt(prompt: &McpPrompt) -> Result<usize, String> {
    if prompt.messages.is_empty() || prompt.messages.len() > MAX_PROMPT_MESSAGES {
        return Err("MCP prompts/get returned an invalid message count".into());
    }
    validate_optional_text(
        "prompt description",
        prompt.description.as_deref(),
        MAX_FEATURE_DESCRIPTION_BYTES,
    )?;
    let mut aggregate_bytes = 0usize;
    for message in &prompt.messages {
        if !matches!(message.role.as_str(), "user" | "assistant") {
            return Err("MCP prompt returned an unsupported message role".into());
        }
        let size = match &message.content {
            McpPromptContent::Text { text } => {
                if text.len() > MAX_PROMPT_CONTENT_BYTES {
                    return Err("MCP prompt text exceeded the byte limit".into());
                }
                text.len()
            }
            McpPromptContent::Image { data, mime_type }
            | McpPromptContent::Audio { data, mime_type } => {
                validate_identity("prompt MIME type", mime_type, MAX_FEATURE_MIME_BYTES)?;
                let decoded = base64::engine::general_purpose::STANDARD
                    .decode(data)
                    .map_err(|_| "MCP prompt returned invalid Base64 content".to_string())?;
                if decoded.len() > MAX_PROMPT_CONTENT_BYTES {
                    return Err("MCP prompt binary content exceeded the byte limit".into());
                }
                decoded.len()
            }
            McpPromptContent::ResourceLink {
                uri,
                name,
                title,
                description,
                mime_type,
                ..
            } => {
                validate_identity("prompt resource URI", uri, MAX_FEATURE_URI_BYTES)?;
                validate_identity("prompt resource name", name, MAX_FEATURE_NAME_BYTES)?;
                validate_optional_text(
                    "prompt resource title",
                    title.as_deref(),
                    MAX_FEATURE_NAME_BYTES,
                )?;
                validate_optional_text(
                    "prompt resource description",
                    description.as_deref(),
                    MAX_FEATURE_DESCRIPTION_BYTES,
                )?;
                validate_optional_text(
                    "prompt resource MIME type",
                    mime_type.as_deref(),
                    MAX_FEATURE_MIME_BYTES,
                )?;
                0
            }
            McpPromptContent::Resource { resource } => {
                validate_resource_contents(std::slice::from_ref(resource))?
            }
        };
        aggregate_bytes = aggregate_bytes.saturating_add(size);
        if aggregate_bytes > MAX_PROMPT_CONTENT_BYTES {
            return Err("MCP prompt exceeded the aggregate content byte limit".into());
        }
    }
    if serde_json::to_vec(prompt)
        .map_err(|_| "MCP prompt could not be encoded for budget validation".to_string())?
        .len()
        > MAX_PROMPT_CONTENT_BYTES
    {
        return Err("MCP prompt exceeded the serialized content byte limit".into());
    }
    Ok(aggregate_bytes)
}

pub(crate) fn validate_prompt_arguments(
    prompt: &McpPromptInfo,
    arguments: &std::collections::HashMap<String, String>,
) -> Result<(), String> {
    let declared = prompt
        .arguments
        .iter()
        .map(|argument| (argument.name.as_str(), argument.required))
        .collect::<std::collections::HashMap<_, _>>();
    if arguments.len() > MAX_PROMPT_ARGUMENTS {
        return Err("MCP prompt argument count exceeded the limit".into());
    }
    let mut total_bytes = 0usize;
    for (name, value) in arguments {
        if !declared.contains_key(name.as_str()) {
            return Err("MCP prompt arguments contain an undeclared name".into());
        }
        validate_text(
            "prompt argument value",
            value,
            MAX_PROMPT_ARGUMENT_BYTES,
            true,
        )?;
        total_bytes = total_bytes
            .saturating_add(name.len())
            .saturating_add(value.len());
        if total_bytes > MAX_PROMPT_CONTENT_BYTES {
            return Err("MCP prompt arguments exceeded the aggregate byte limit".into());
        }
    }
    if declared
        .iter()
        .any(|(name, required)| *required && !arguments.contains_key(*name))
    {
        return Err("MCP prompt is missing a required argument".into());
    }
    Ok(())
}

pub(crate) fn validate_cursor(cursor: Option<&str>) -> Result<(), String> {
    if cursor.is_some_and(|value| value.is_empty() || value.len() > MAX_FEATURE_CURSOR_BYTES) {
        return Err("MCP returned an invalid pagination cursor".into());
    }
    Ok(())
}

pub(crate) fn validate_uri(uri: &str) -> Result<(), String> {
    validate_identity("resource URI", uri, MAX_FEATURE_URI_BYTES)
}

pub(crate) fn validate_identity(label: &str, value: &str, max_bytes: usize) -> Result<(), String> {
    validate_text(label, value, max_bytes, false)?;
    if value.chars().any(char::is_control) {
        return Err(format!("MCP {label} is invalid"));
    }
    Ok(())
}

fn validate_optional_text(
    label: &str,
    value: Option<&str>,
    max_bytes: usize,
) -> Result<(), String> {
    if let Some(value) = value {
        validate_text(label, value, max_bytes, true)?;
    }
    Ok(())
}

fn validate_text(
    label: &str,
    value: &str,
    max_bytes: usize,
    allow_empty: bool,
) -> Result<(), String> {
    if (!allow_empty && value.is_empty())
        || value.len() > max_bytes
        || value
            .chars()
            .any(|character| character.is_control() && !matches!(character, '\n' | '\r' | '\t'))
    {
        return Err(format!("MCP {label} is invalid"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn complete_result_is_required_and_mrtr_is_rejected_without_echoing_state() {
        assert!(validate_complete_result(&serde_json::json!({}), "resources/read").is_err());
        assert!(
            validate_complete_result(
                &serde_json::json!({"resultType": "complete"}),
                "resources/read"
            )
            .is_ok()
        );
        let error = validate_complete_result(
            &serde_json::json!({
                "resultType": "input_required",
                "requestState": "private-server-state",
                "inputRequests": {"secret": {"method": "elicitation/create"}}
            }),
            "resources/read",
        )
        .unwrap_err();
        assert!(error.contains("requires additional interaction"));
        assert!(!error.contains("private-server-state"));
        assert!(validate_complete_result(&serde_json::json!({}), "prompts/get").is_err());
    }

    #[test]
    fn current_capabilities_are_detected_from_discovery() {
        let modern = McpServerCapabilities::from_wire_capabilities(Some(&serde_json::json!({
            "resources": {"listChanged": true, "subscribe": true},
            "prompts": {}
        })));
        assert!(!modern.tools);
        assert!(modern.resources);
        assert!(modern.resource_subscribe);
        assert!(modern.prompts);

        assert_eq!(
            McpServerCapabilities::from_wire_capabilities(None),
            McpServerCapabilities::default()
        );
    }

    #[test]
    fn resource_contents_require_one_bounded_valid_payload() {
        let valid = McpResourceContent {
            uri: "mcp://server/item".into(),
            mime_type: Some("text/plain".into()),
            text: Some("hello\nworld".into()),
            blob: None,
        };
        assert_eq!(validate_resource_contents(&[valid]).unwrap(), 11);

        let both = McpResourceContent {
            uri: "mcp://server/item".into(),
            mime_type: None,
            text: Some("text".into()),
            blob: Some("dGV4dA==".into()),
        };
        assert!(validate_resource_contents(&[both]).is_err());

        let malformed = McpResourceContent {
            uri: "mcp://server/item".into(),
            mime_type: None,
            text: None,
            blob: Some("not-base64".into()),
        };
        assert!(validate_resource_contents(&[malformed]).is_err());
    }

    #[test]
    fn resource_templates_require_bounded_opaque_templates_and_metadata() {
        let valid = McpResourceTemplate {
            uri_template: "mcp://server/items/{item_id}{?limit}".into(),
            name: "Items".into(),
            title: Some("Select an item".into()),
            description: None,
            mime_type: Some("application/json".into()),
        };
        assert!(validate_resource_template(&valid).is_ok());

        let invalid = McpResourceTemplate {
            uri_template: "".into(),
            name: "Items".into(),
            title: None,
            description: None,
            mime_type: None,
        };
        assert!(validate_resource_template(&invalid).is_err());
    }

    #[test]
    fn prompt_arguments_and_content_are_bounded_and_typed() {
        let info = McpPromptInfo {
            name: "review".into(),
            title: None,
            description: None,
            arguments: vec![McpPromptArgument {
                name: "code".into(),
                title: None,
                description: None,
                required: true,
            }],
        };
        validate_prompt_arguments(
            &info,
            &HashMap::from([("code".into(), "fn main() {}".into())]),
        )
        .unwrap();
        assert!(validate_prompt_arguments(&info, &HashMap::new()).is_err());
        assert!(
            validate_prompt_arguments(&info, &HashMap::from([("other".into(), "value".into())]))
                .is_err()
        );

        let prompt = McpPrompt {
            description: Some("Review source".into()),
            messages: vec![McpPromptMessage {
                role: "user".into(),
                content: McpPromptContent::Text {
                    text: "Please review this source.".into(),
                },
            }],
            ttl_ms: Some(1000),
            cache_scope: McpCacheScope::Private,
        };
        validate_prompt(&prompt).unwrap();
        assert!(
            serde_json::from_value::<McpPromptMessage>(serde_json::json!({
                "role": "user",
                "content": {"type": "unknown", "payload": "x"}
            }))
            .is_err()
        );
    }
}
