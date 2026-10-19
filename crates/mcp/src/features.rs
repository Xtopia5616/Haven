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
pub(crate) const MAX_MCP_INPUT_REQUESTS: usize = 8;
pub(crate) const MAX_MCP_INPUT_FIELDS: usize = 32;
pub(crate) const MAX_MCP_INPUT_REQUEST_BYTES: usize = 64 * 1024;
pub(crate) const MAX_MCP_INPUT_REQUIRED_BYTES: usize = 256 * 1024;
pub(crate) const MAX_MCP_INPUT_MESSAGE_BYTES: usize = 4096;
pub(crate) const MAX_MCP_INPUT_FIELD_BYTES: usize = 256;
pub(crate) const MAX_MCP_INPUT_VALUE_BYTES: usize = 16 * 1024;
const MAX_MCP_INPUT_SAFE_INTEGER: i64 = 9_007_199_254_740_991;
pub(crate) const MAX_MCP_MRTR_ROUNDS: u32 = 8;
pub(crate) const MAX_PENDING_MCP_FEATURE_INPUTS: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum McpFeatureCallStatus {
    Complete,
    InputRequired,
}

/// A bounded interaction result for a user-selected resource read or prompt.
/// Only one result payload is present for each status: `resource_read` or
/// `prompt` for `complete`, and `request_id`, `round`, and `requests` for
/// `input_required`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct McpFeatureCallOutcome {
    pub status: McpFeatureCallStatus,
    pub resource_read: Option<McpResourceRead>,
    pub prompt: Option<McpPrompt>,
    pub request_id: Option<u64>,
    pub round: Option<u32>,
    pub requests: Vec<McpElicitationRequest>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum McpElicitationFieldType {
    String,
    Number,
    Integer,
    Boolean,
}

/// One primitive field from a supported form-mode elicitation schema.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct McpElicitationField {
    pub name: String,
    pub title: Option<String>,
    pub description: Option<String>,
    pub field_type: McpElicitationFieldType,
    pub required: bool,
    pub options: Vec<String>,
    pub minimum: Option<f64>,
    pub maximum: Option<f64>,
    pub min_length: Option<usize>,
    pub max_length: Option<usize>,
}

/// A server-originated request, normalized for a safe, plain UI form.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct McpElicitationRequest {
    pub key: String,
    pub message: String,
    pub fields: Vec<McpElicitationField>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum McpElicitationAction {
    Accept,
    Decline,
}

/// User answer to a single form elicitation. Values are protocol data and are
/// validated against the server's bounded schema before the retry is sent.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpElicitationAnswer {
    pub action: McpElicitationAction,
    #[serde(default)]
    pub content: std::collections::HashMap<String, Value>,
}

pub(crate) struct ParsedInputRequired {
    pub(crate) requests: Vec<McpElicitationRequest>,
    pub(crate) request_state: Option<String>,
}

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

pub(crate) fn parse_input_required(result: &Value) -> Result<ParsedInputRequired, String> {
    let response_bytes = serde_json::to_vec(result)
        .map_err(|_| "MCP input_required response could not be measured".to_string())?
        .len();
    if response_bytes > MAX_MCP_INPUT_REQUIRED_BYTES {
        return Err("MCP input_required response exceeded its aggregate byte limit".into());
    }
    let Some(result_state) = result.get("resultType").and_then(Value::as_str) else {
        return Err("MCP input-required result omitted its result type".into());
    };
    if result_state != "input_required" {
        return Err("MCP result is not input_required".into());
    }

    let request_state = match result.get("requestState") {
        None => None,
        Some(Value::String(value)) if value.len() <= MAX_MCP_INPUT_REQUEST_BYTES => {
            Some(value.clone())
        }
        Some(_) => return Err("MCP requestState was invalid or exceeded its byte limit".into()),
    };
    let mut requests = Vec::new();
    if let Some(input_requests) = result.get("inputRequests") {
        let Some(input_requests) = input_requests.as_object() else {
            return Err("MCP inputRequests was not an object".into());
        };
        if input_requests.len() > MAX_MCP_INPUT_REQUESTS {
            return Err("MCP input_required exceeded the interaction request limit".into());
        }
        for (key, request) in input_requests {
            validate_identity("MCP input request key", key, MAX_MCP_INPUT_FIELD_BYTES)?;
            let method = request
                .get("method")
                .and_then(Value::as_str)
                .ok_or_else(|| "MCP input request omitted its method".to_string())?;
            if method != "elicitation/create" {
                return Err(format!(
                    "MCP input request method '{method}' is not supported"
                ));
            }
            let params = request
                .get("params")
                .and_then(Value::as_object)
                .ok_or_else(|| "MCP elicitation request omitted its parameters".to_string())?;
            if params.get("mode").and_then(Value::as_str) != Some("form") {
                return Err("MCP URL-mode elicitation is not supported".into());
            }
            let message = params
                .get("message")
                .and_then(Value::as_str)
                .ok_or_else(|| "MCP elicitation request omitted its message".to_string())?;
            validate_identity(
                "MCP elicitation message",
                message,
                MAX_MCP_INPUT_MESSAGE_BYTES,
            )?;
            let schema = params.get("requestedSchema").ok_or_else(|| {
                "MCP elicitation request omitted its requested schema".to_string()
            })?;
            requests.push(McpElicitationRequest {
                key: key.clone(),
                message: message.to_string(),
                fields: parse_form_schema(schema)?,
            });
        }
    }
    if requests.is_empty() && request_state.is_none() {
        return Err("MCP input_required omitted both inputRequests and requestState".into());
    }
    Ok(ParsedInputRequired {
        requests,
        request_state,
    })
}

fn parse_form_schema(schema: &Value) -> Result<Vec<McpElicitationField>, String> {
    let Some(schema) = schema.as_object() else {
        return Err("MCP elicitation requestedSchema must be an object".into());
    };
    for key in schema.keys() {
        if !matches!(key.as_str(), "type" | "properties" | "required") {
            return Err(format!(
                "MCP elicitation schema keyword '{key}' is not supported"
            ));
        }
    }
    if schema.get("type").and_then(Value::as_str) != Some("object") {
        return Err("MCP elicitation requestedSchema must have type object".into());
    }
    let properties = schema
        .get("properties")
        .and_then(Value::as_object)
        .ok_or_else(|| "MCP elicitation requestedSchema omitted properties".to_string())?;
    if properties.len() > MAX_MCP_INPUT_FIELDS {
        return Err("MCP elicitation requestedSchema exceeded the field limit".into());
    }
    let mut required = std::collections::HashSet::new();
    if let Some(required_value) = schema.get("required") {
        let Some(names) = required_value.as_array() else {
            return Err("MCP elicitation requestedSchema required must be an array".into());
        };
        if names.len() > MAX_MCP_INPUT_FIELDS {
            return Err("MCP elicitation requestedSchema exceeded the required-field limit".into());
        }
        for name in names {
            let Some(name) = name.as_str() else {
                return Err("MCP elicitation required-field names must be strings".into());
            };
            validate_identity(
                "MCP elicitation field name",
                name,
                MAX_MCP_INPUT_FIELD_BYTES,
            )?;
            if !required.insert(name.to_string()) {
                return Err("MCP elicitation requestedSchema repeated a required field".into());
            }
            if !properties.contains_key(name) {
                return Err("MCP elicitation required field was not declared".into());
            }
        }
    }

    let mut fields = Vec::with_capacity(properties.len());
    for (name, property) in properties {
        validate_identity(
            "MCP elicitation field name",
            name,
            MAX_MCP_INPUT_FIELD_BYTES,
        )?;
        let Some(property) = property.as_object() else {
            return Err("MCP elicitation field schema must be an object".into());
        };
        for key in property.keys() {
            if !matches!(
                key.as_str(),
                "type"
                    | "title"
                    | "description"
                    | "enum"
                    | "minimum"
                    | "maximum"
                    | "minLength"
                    | "maxLength"
            ) {
                return Err(format!(
                    "MCP elicitation field keyword '{key}' is not supported"
                ));
            }
        }
        let field_type = match property.get("type").and_then(Value::as_str) {
            Some("string") => McpElicitationFieldType::String,
            Some("number") => McpElicitationFieldType::Number,
            Some("integer") => McpElicitationFieldType::Integer,
            Some("boolean") => McpElicitationFieldType::Boolean,
            _ => return Err("MCP elicitation fields must use a supported primitive type".into()),
        };
        let title = optional_form_text(property.get("title"), "MCP elicitation field title")?;
        let description = optional_form_text(
            property.get("description"),
            "MCP elicitation field description",
        )?;
        let minimum = optional_finite_number(property.get("minimum"), "minimum")?;
        let maximum = optional_finite_number(property.get("maximum"), "maximum")?;
        if minimum
            .zip(maximum)
            .is_some_and(|(minimum, maximum)| minimum > maximum)
        {
            return Err("MCP elicitation numeric bounds are inverted".into());
        }
        if !matches!(
            field_type,
            McpElicitationFieldType::Number | McpElicitationFieldType::Integer
        ) && (minimum.is_some() || maximum.is_some())
        {
            return Err("MCP elicitation numeric bounds require a number or integer field".into());
        }
        if field_type == McpElicitationFieldType::Integer
            && [minimum, maximum].into_iter().flatten().any(|bound| {
                bound.fract() != 0.0 || bound.abs() > MAX_MCP_INPUT_SAFE_INTEGER as f64
            })
        {
            return Err("MCP elicitation integer bounds exceed the safe integer range".into());
        }
        let min_length = optional_length(property.get("minLength"), "minLength")?;
        let max_length = optional_length(property.get("maxLength"), "maxLength")?;
        if min_length
            .zip(max_length)
            .is_some_and(|(minimum, maximum)| minimum > maximum)
        {
            return Err("MCP elicitation string length bounds are inverted".into());
        }
        if !matches!(field_type, McpElicitationFieldType::String)
            && (min_length.is_some() || max_length.is_some())
        {
            return Err("MCP elicitation string length bounds require a string field".into());
        }
        let options = if let Some(options) = property.get("enum") {
            if field_type != McpElicitationFieldType::String {
                return Err("MCP elicitation enum is supported only for string fields".into());
            }
            let Some(options) = options.as_array() else {
                return Err("MCP elicitation enum must be an array".into());
            };
            if options.is_empty() || options.len() > 64 {
                return Err("MCP elicitation enum exceeded its option limit".into());
            }
            let mut values = Vec::with_capacity(options.len());
            for option in options {
                let Some(option) = option.as_str() else {
                    return Err("MCP elicitation enum values must be strings".into());
                };
                validate_identity(
                    "MCP elicitation enum value",
                    option,
                    MAX_MCP_INPUT_VALUE_BYTES,
                )?;
                if values.iter().any(|value| value == option) {
                    return Err("MCP elicitation enum repeated an option".into());
                }
                values.push(option.to_string());
            }
            values
        } else {
            Vec::new()
        };
        fields.push(McpElicitationField {
            name: name.clone(),
            title,
            description,
            field_type,
            required: required.contains(name),
            options,
            minimum,
            maximum,
            min_length,
            max_length,
        });
    }
    Ok(fields)
}

fn optional_form_text(value: Option<&Value>, label: &str) -> Result<Option<String>, String> {
    let Some(value) = value else {
        return Ok(None);
    };
    let Some(value) = value.as_str() else {
        return Err(format!("{label} must be a string"));
    };
    validate_identity(label, value, MAX_MCP_INPUT_MESSAGE_BYTES)?;
    Ok(Some(value.to_string()))
}

fn optional_finite_number(value: Option<&Value>, label: &str) -> Result<Option<f64>, String> {
    let Some(value) = value else {
        return Ok(None);
    };
    let Some(value) = value.as_f64().filter(|value| value.is_finite()) else {
        return Err(format!("MCP elicitation {label} must be a finite number"));
    };
    Ok(Some(value))
}

fn optional_length(value: Option<&Value>, label: &str) -> Result<Option<usize>, String> {
    let Some(value) = value else {
        return Ok(None);
    };
    let Some(value) = value.as_u64().and_then(|value| usize::try_from(value).ok()) else {
        return Err(format!(
            "MCP elicitation {label} must be a non-negative integer"
        ));
    };
    if value > MAX_MCP_INPUT_VALUE_BYTES {
        return Err(format!("MCP elicitation {label} exceeded its bound"));
    }
    Ok(Some(value))
}

pub(crate) fn validate_input_responses(
    requests: &[McpElicitationRequest],
    answers: &std::collections::HashMap<String, McpElicitationAnswer>,
) -> Result<Value, String> {
    if answers.len() != requests.len() || requests.is_empty() {
        return Err("MCP input responses must answer every current request exactly once".into());
    }
    let mut input_responses = serde_json::Map::new();
    for request in requests {
        let Some(answer) = answers.get(&request.key) else {
            return Err("MCP input response omitted a requested key".into());
        };
        match answer.action {
            McpElicitationAction::Decline => {
                if !answer.content.is_empty() {
                    return Err("declined MCP elicitation must not include content".into());
                }
                input_responses.insert(
                    request.key.clone(),
                    serde_json::json!({"action": "decline"}),
                );
            }
            McpElicitationAction::Accept => {
                let mut content = serde_json::Map::new();
                for field in &request.fields {
                    let Some(value) = answer.content.get(&field.name) else {
                        if field.required {
                            return Err(format!("required MCP input '{}' is missing", field.name));
                        }
                        continue;
                    };
                    validate_input_value(field, value)?;
                    content.insert(field.name.clone(), value.clone());
                }
                if answer
                    .content
                    .keys()
                    .any(|name| !request.fields.iter().any(|field| &field.name == name))
                {
                    return Err("MCP input response included an undeclared field".into());
                }
                input_responses.insert(
                    request.key.clone(),
                    serde_json::json!({"action": "accept", "content": content}),
                );
            }
        }
    }
    let input_responses = Value::Object(input_responses);
    let serialized_bytes = serde_json::to_vec(&input_responses)
        .map_err(|_| "MCP input responses could not be measured".to_string())?
        .len();
    if serialized_bytes > MAX_MCP_INPUT_REQUEST_BYTES {
        return Err("MCP input responses exceeded the aggregate byte limit".into());
    }
    Ok(input_responses)
}

fn validate_input_value(field: &McpElicitationField, value: &Value) -> Result<(), String> {
    match field.field_type {
        McpElicitationFieldType::String => {
            let Some(value) = value.as_str() else {
                return Err(format!("MCP input '{}' must be a string", field.name));
            };
            if value.len() > MAX_MCP_INPUT_VALUE_BYTES
                || field
                    .min_length
                    .is_some_and(|minimum| value.chars().count() < minimum)
                || field
                    .max_length
                    .is_some_and(|maximum| value.chars().count() > maximum)
            {
                return Err(format!(
                    "MCP input '{}' has an invalid string length",
                    field.name
                ));
            }
            if !field.options.is_empty() && !field.options.iter().any(|option| option == value) {
                return Err(format!(
                    "MCP input '{}' is not an allowed option",
                    field.name
                ));
            }
        }
        McpElicitationFieldType::Integer => {
            let Some(value) = value
                .as_i64()
                .filter(|value| value.unsigned_abs() <= MAX_MCP_INPUT_SAFE_INTEGER as u64)
            else {
                return Err(format!("MCP input '{}' must be an integer", field.name));
            };
            validate_numeric_range(field, value as f64)?;
        }
        McpElicitationFieldType::Number => {
            let Some(value) = value.as_f64().filter(|value| value.is_finite()) else {
                return Err(format!(
                    "MCP input '{}' must be a finite number",
                    field.name
                ));
            };
            validate_numeric_range(field, value)?;
        }
        McpElicitationFieldType::Boolean => {
            if !value.is_boolean() {
                return Err(format!("MCP input '{}' must be a boolean", field.name));
            }
        }
    }
    Ok(())
}

fn validate_numeric_range(field: &McpElicitationField, value: f64) -> Result<(), String> {
    if field.minimum.is_some_and(|minimum| value < minimum)
        || field.maximum.is_some_and(|maximum| value > maximum)
    {
        return Err(format!(
            "MCP input '{}' is outside its allowed range",
            field.name
        ));
    }
    Ok(())
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
    fn parses_bounded_form_elicitation_and_validates_answers() {
        let result = serde_json::json!({
            "resultType": "input_required",
            "requestState": " opaque state ",
            "inputRequests": {
                "choose-topic": {
                    "method": "elicitation/create",
                    "params": {
                        "mode": "form",
                        "message": "Choose a topic",
                        "requestedSchema": {
                            "type": "object",
                            "properties": {
                                "topic": {"type": "string", "enum": ["rust", "mcp"]},
                                "count": {"type": "integer", "minimum": 1, "maximum": 5}
                            },
                            "required": ["topic"]
                        }
                    }
                }
            }
        });
        let parsed = parse_input_required(&result).unwrap();
        assert_eq!(parsed.request_state.as_deref(), Some(" opaque state "));
        assert_eq!(parsed.requests.len(), 1);
        assert_eq!(parsed.requests[0].fields.len(), 2);
        let topic = parsed.requests[0]
            .fields
            .iter()
            .find(|field| field.name == "topic")
            .unwrap();
        assert!(topic.required);
        assert_eq!(topic.options, ["rust", "mcp"]);

        let answers = HashMap::from([(
            "choose-topic".into(),
            McpElicitationAnswer {
                action: McpElicitationAction::Accept,
                content: HashMap::from([
                    ("topic".into(), Value::String("rust".into())),
                    ("count".into(), Value::Number(3.into())),
                ]),
            },
        )]);
        let response = validate_input_responses(&parsed.requests, &answers).unwrap();
        assert_eq!(response["choose-topic"]["action"], "accept");
        assert_eq!(response["choose-topic"]["content"]["topic"], "rust");
        assert_eq!(response["choose-topic"]["content"]["count"], 3);
    }

    #[test]
    fn rejects_unsupported_or_malformed_form_elicitation() {
        let base = serde_json::json!({
            "resultType": "input_required",
            "inputRequests": {
                "request": {
                    "method": "elicitation/create",
                    "params": {
                        "mode": "form",
                        "message": "Enter a value",
                        "requestedSchema": {"type": "object", "properties": {}}
                    }
                }
            }
        });
        assert!(parse_input_required(&base).is_ok());

        let mut url_mode = base.clone();
        url_mode["inputRequests"]["request"]["params"]["mode"] = "url".into();
        assert!(parse_input_required(&url_mode).is_err());

        let mut unsupported_method = base.clone();
        unsupported_method["inputRequests"]["request"]["method"] = "sampling/createMessage".into();
        assert!(parse_input_required(&unsupported_method).is_err());

        let mut unsupported_type = base;
        unsupported_type["inputRequests"]["request"]["params"]["requestedSchema"]["properties"] =
            serde_json::json!({"file": {"type": "object"}});
        assert!(parse_input_required(&unsupported_type).is_err());
    }

    #[test]
    fn requires_exact_responses_and_validates_schema_values() {
        let parsed = parse_input_required(&serde_json::json!({
            "resultType": "input_required",
            "inputRequests": {
                "one": {
                    "method": "elicitation/create",
                    "params": {
                        "mode": "form",
                        "message": "Answer",
                        "requestedSchema": {
                            "type": "object",
                            "properties": {
                                "name": {"type": "string", "minLength": 2, "maxLength": 5},
                                "enabled": {"type": "boolean"}
                            },
                            "required": ["name"]
                        }
                    }
                }
            }
        }))
        .unwrap();
        let answer = |content| {
            HashMap::from([(
                "one".into(),
                McpElicitationAnswer {
                    action: McpElicitationAction::Accept,
                    content,
                },
            )])
        };
        assert!(validate_input_responses(&parsed.requests, &HashMap::new()).is_err());
        assert!(validate_input_responses(&parsed.requests, &answer(HashMap::new())).is_err());
        assert!(
            validate_input_responses(
                &parsed.requests,
                &answer(HashMap::from([("unknown".into(), Value::Null)]))
            )
            .is_err()
        );
        assert!(
            validate_input_responses(
                &parsed.requests,
                &answer(HashMap::from([("name".into(), Value::String("x".into()))]))
            )
            .is_err()
        );

        let declined = HashMap::from([(
            "one".into(),
            McpElicitationAnswer {
                action: McpElicitationAction::Decline,
                content: HashMap::new(),
            },
        )]);
        assert_eq!(
            validate_input_responses(&parsed.requests, &declined).unwrap()["one"]["action"],
            "decline"
        );
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
