use haven_common::config::{McpExposureConfig, McpStdioExecution};
use haven_common::mcp::McpServerTransport;
use serde_json::Value;
use std::error::Error;
use std::fmt;

pub(crate) const MODERN_PROTOCOL_VERSION: &str = "2026-07-28";
pub(crate) const LEGACY_PROTOCOL_VERSION: &str = "2024-11-05";
pub(crate) const REQUEST_TIMEOUT_SECS: u64 = 30;

const MCP_META_PREFIX: &str = "io.modelcontextprotocol/";
const MCP_RPC_HEADER_MISMATCH: i64 = -32020;
const MCP_RPC_MISSING_CLIENT_CAPABILITY: i64 = -32021;
const MCP_RPC_UNSUPPORTED_PROTOCOL_VERSION: i64 = -32022;

/// Wire behavior selected for a peer. The protocol version belongs to the
/// MCP peer, not to Haven's own session identity.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) enum McpProtocolMode {
    #[default]
    Uninitialized,
    Modern {
        version: String,
    },
    Legacy {
        version: String,
    },
}

impl McpProtocolMode {
    pub(crate) fn modern(version: impl Into<String>) -> Self {
        Self::Modern {
            version: version.into(),
        }
    }

    pub(crate) fn legacy(version: impl Into<String>) -> Self {
        Self::Legacy {
            version: version.into(),
        }
    }

    pub(crate) fn is_modern(&self) -> bool {
        matches!(self, Self::Modern { .. })
    }

    pub(crate) fn is_legacy(&self) -> bool {
        matches!(self, Self::Legacy { .. })
    }
}

/// Modern MCP carries client identity, capabilities, and protocol version on
/// every request. This helper is also used for the `server/discover` probe.
pub(crate) fn params_with_protocol_meta(params: Option<Value>, version: &str) -> Value {
    let mut params = match params {
        Some(Value::Object(params)) => params,
        Some(_) | None => serde_json::Map::new(),
    };
    params.insert(
        "_meta".into(),
        serde_json::json!({
            format!("{MCP_META_PREFIX}protocolVersion"): version,
            format!("{MCP_META_PREFIX}clientInfo"): {
                "name": "Haven",
                "version": env!("CARGO_PKG_VERSION"),
            },
            format!("{MCP_META_PREFIX}clientCapabilities"): {},
        }),
    );
    Value::Object(params)
}

/// Structured protocol error retained long enough for transport-era
/// detection. Display text intentionally excludes server-controlled details.
#[derive(Debug)]
pub(crate) struct McpRpcError {
    pub(crate) code: i64,
    pub(crate) data: Option<Value>,
    pub(crate) http_status: Option<u16>,
}

impl McpRpcError {
    pub(crate) fn from_response(value: &Value, http_status: Option<u16>) -> Option<Self> {
        if value.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
            return None;
        }
        let error = value.get("error")?;
        Some(Self {
            code: error.get("code")?.as_i64()?,
            data: error.get("data").cloned(),
            http_status,
        })
    }

    pub(crate) fn is_recognized_modern(&self) -> bool {
        matches!(
            self.code,
            MCP_RPC_HEADER_MISMATCH
                | MCP_RPC_MISSING_CLIENT_CAPABILITY
                | MCP_RPC_UNSUPPORTED_PROTOCOL_VERSION
        ) || (self.http_status == Some(404) && self.code == -32601)
    }
}

impl fmt::Display for McpRpcError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "MCP JSON-RPC request failed (code {})", self.code)
    }
}

impl Error for McpRpcError {}

#[derive(Debug)]
pub(crate) struct McpHttpStatusError {
    pub(crate) status: u16,
}

impl fmt::Display for McpHttpStatusError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "MCP HTTP request failed (status {})", self.status)
    }
}

impl Error for McpHttpStatusError {}

pub(crate) fn should_fallback_to_legacy(error: &anyhow::Error, is_http: bool) -> bool {
    if error
        .downcast_ref::<McpRpcError>()
        .is_some_and(McpRpcError::is_recognized_modern)
    {
        return false;
    }
    if !is_http {
        return true;
    }
    error
        .downcast_ref::<McpHttpStatusError>()
        .is_some_and(|error| matches!(error.status, 400 | 404 | 405))
        || error
            .downcast_ref::<McpRpcError>()
            .is_some_and(|error| matches!(error.http_status, Some(400 | 404 | 405)))
}

pub(crate) fn supported_legacy_version(error: &anyhow::Error) -> Option<&'static str> {
    let rpc_error = error.downcast_ref::<McpRpcError>()?;
    if rpc_error.code != MCP_RPC_UNSUPPORTED_PROTOCOL_VERSION {
        return None;
    }
    let supported = rpc_error.data.as_ref()?.get("supported")?.as_array()?;
    [
        "2025-11-25",
        "2025-06-18",
        "2025-03-26",
        LEGACY_PROTOCOL_VERSION,
    ]
    .into_iter()
    .find(|version| {
        supported
            .iter()
            .any(|candidate| candidate.as_str() == Some(*version))
    })
}

/// Encode an HTTP routing-header value per Streamable HTTP. Header values
/// outside the safe visible-ASCII set, surrounding whitespace, or the
/// reserved sentinel syntax are carried as UTF-8 Base64.
pub(crate) fn encode_mcp_header_value(value: &str) -> String {
    let safe_ascii = value.bytes().all(|byte| (0x20..=0x7e).contains(&byte));
    let trimmed = value.trim_matches([' ', '\t']);
    let sentinel = value.starts_with("=?base64?") && value.ends_with("?=");
    if safe_ascii && value == trimmed && !sentinel {
        value.to_string()
    } else {
        use base64::Engine;
        format!(
            "=?base64?{}?=",
            base64::engine::general_purpose::STANDARD.encode(value.as_bytes())
        )
    }
}

#[derive(Debug)]
struct McpParameterHeader {
    name: String,
    path: Vec<String>,
    value_type: String,
}

/// Validate the 2026 HTTP parameter-header extension and return its headers
/// for the supplied arguments. Only paths made entirely of `properties`
/// entries are accepted, matching the protocol's static-path rule.
pub(crate) fn http_parameter_headers(
    schema: &Value,
    arguments: &Value,
) -> Result<Vec<(String, String)>, String> {
    let mut definitions = Vec::new();
    let mut names = std::collections::HashSet::new();
    collect_parameter_headers(schema, &mut Vec::new(), true, &mut definitions, &mut names)?;

    let mut result = Vec::with_capacity(definitions.len());
    for definition in definitions {
        let mut value = arguments;
        let mut present = true;
        for key in &definition.path {
            let Some(next) = value.get(key) else {
                present = false;
                break;
            };
            value = next;
        }
        if present && !value.is_null() {
            result.push(encode_parameter_header(definition, value)?);
        }
    }
    Ok(result)
}

/// Wire page returned by `tools/list`. New protocol revisions add pagination
/// and cache hints while legacy peers may omit all three fields.
#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct McpToolListPage {
    pub(crate) tools: Vec<McpToolInfo>,
    #[serde(default)]
    pub(crate) next_cursor: Option<String>,
    #[serde(default)]
    pub(crate) ttl_ms: Option<u64>,
    #[serde(default)]
    pub(crate) cache_scope: Option<String>,
}

fn collect_parameter_headers(
    value: &Value,
    path: &mut Vec<String>,
    static_properties_path: bool,
    definitions: &mut Vec<McpParameterHeader>,
    names: &mut std::collections::HashSet<String>,
) -> Result<(), String> {
    match value {
        Value::Object(object) => {
            if let Some(header_name) = object.get("x-mcp-header") {
                let header_name = header_name
                    .as_str()
                    .ok_or_else(|| "x-mcp-header must be a string".to_string())?;
                if !static_properties_path || path.is_empty() {
                    return Err("x-mcp-header must be inside a static properties path".into());
                }
                if header_name.is_empty() || !header_name.bytes().all(is_http_token_byte) {
                    return Err("x-mcp-header must be a valid HTTP field-name token".into());
                }
                let value_type = object
                    .get("type")
                    .and_then(Value::as_str)
                    .filter(|kind| matches!(*kind, "string" | "integer" | "boolean"))
                    .ok_or_else(|| {
                        "x-mcp-header is allowed only for string, integer, or boolean properties"
                            .to_string()
                    })?;
                if !names.insert(header_name.to_ascii_lowercase()) {
                    return Err("x-mcp-header values must be unique ignoring case".into());
                }
                definitions.push(McpParameterHeader {
                    name: header_name.to_string(),
                    path: path.clone(),
                    value_type: value_type.to_string(),
                });
            }

            for (key, child) in object {
                if key == "properties" {
                    if let Some(properties) = child.as_object() {
                        for (property_name, property_schema) in properties {
                            path.push(property_name.clone());
                            collect_parameter_headers(
                                property_schema,
                                path,
                                static_properties_path,
                                definitions,
                                names,
                            )?;
                            path.pop();
                        }
                    } else {
                        collect_parameter_headers(child, path, false, definitions, names)?;
                    }
                } else {
                    collect_parameter_headers(child, path, false, definitions, names)?;
                }
            }
            Ok(())
        }
        Value::Array(values) => {
            for child in values {
                collect_parameter_headers(child, path, false, definitions, names)?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

fn is_http_token_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&byte)
}

fn encode_parameter_header(
    definition: McpParameterHeader,
    value: &Value,
) -> Result<(String, String), String> {
    let raw = match definition.value_type.as_str() {
        "string" => value
            .as_str()
            .ok_or_else(|| "MCP tool argument does not match its string header schema".to_string())?
            .to_string(),
        "boolean" => value
            .as_bool()
            .ok_or_else(|| {
                "MCP tool argument does not match its boolean header schema".to_string()
            })?
            .to_string(),
        "integer" => {
            const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;
            if let Some(number) = value.as_i64() {
                if number.unsigned_abs() > MAX_SAFE_INTEGER {
                    return Err("MCP integer header argument exceeds the safe integer range".into());
                }
                number.to_string()
            } else if let Some(number) = value.as_u64() {
                if number > MAX_SAFE_INTEGER {
                    return Err("MCP integer header argument exceeds the safe integer range".into());
                }
                number.to_string()
            } else {
                return Err("MCP tool argument does not match its integer header schema".into());
            }
        }
        _ => return Err("unsupported MCP parameter header type".into()),
    };
    Ok((
        format!("Mcp-Param-{}", definition.name),
        encode_mcp_header_value(&raw),
    ))
}

// ---------------------------------------------------------------------------
// Neutral tool-call output
// ---------------------------------------------------------------------------

/// Result of an MCP `tools/call`, decoupled from any tool-execution crate.
/// Tool adapters (e.g. in `haven-tools`) map this into their own result type
/// without the MCP client knowing about it.
#[derive(Debug, Clone)]
pub struct McpCallOutput {
    pub success: bool,
    pub output: serde_json::Value,
    pub error: Option<String>,
}

// ---------------------------------------------------------------------------
// JSON-RPC 2.0 helpers
// ---------------------------------------------------------------------------

pub(crate) fn jsonrpc_request(id: u64, method: &str, params: Option<Value>) -> Value {
    let mut req = serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": method,
    });
    if let Some(p) = params {
        req["params"] = p;
    }
    req
}

pub(crate) fn jsonrpc_notification(method: &str, params: Option<Value>) -> Value {
    let mut req = serde_json::json!({
        "jsonrpc": "2.0",
        "method": method,
    });
    if let Some(p) = params {
        req["params"] = p;
    }
    req
}

// ---------------------------------------------------------------------------
// MCP content block extraction
// ---------------------------------------------------------------------------

/// Approximate decoded length of a base64 payload, computed without allocating
/// the buffer (base64 adds ~1/3 overhead and up to 2 padding `=` bytes for a
/// block-aligned input). Only used for reporting sizes / caps.
fn base64_decoded_len(s: &str) -> usize {
    let bytes = s.as_bytes();
    let len = bytes.len();
    let mut out = len / 4 * 3;
    if len > 0 && bytes[len - 1] == b'=' {
        out -= 1;
    }
    if len > 1 && bytes[len - 2] == b'=' {
        out -= 1;
    }
    out
}

/// Normalize the response of a `tools/call` into a structured `output` object
/// and a plain-text `summary` for the agent loop.
///
/// MCP content blocks may be `text`, `image`, `audio`, or `resource`. Text and
/// text-resources fold into the plain-text summary. Binary media (`image`,
/// `audio`, embedded resource blobs) is surfaced once under `output.images` /
/// `output.audio` / `output.resources` (base64 + mimeType, capped at
/// `max_binary_base64_bytes` bytes of the Base64 representation) for downstream
/// rendering, while
/// `output.content` keeps a metadata-only list of every block (no raw payload)
/// for UI fidelity. Unknown or malformed block types are preserved rather than
/// silently dropped.
pub(crate) struct McpContentExtraction {
    pub(crate) output: Value,
    pub(crate) text_summary: String,
}

pub(crate) fn extract_mcp_content(
    content: &[Value],
    max_binary_base64_bytes: usize,
) -> McpContentExtraction {
    let mut text_parts: Vec<String> = Vec::new();
    let mut images: Vec<Value> = Vec::new();
    let mut audio: Vec<Value> = Vec::new();
    let mut resources: Vec<Value> = Vec::new();
    let mut normalized: Vec<Value> = Vec::new();

    for item in content {
        let kind = match item.get("type").and_then(|t| t.as_str()) {
            Some(k) => k,
            // A type-less block is malformed: preserve it like unknown types.
            None => {
                text_parts.push(serde_json::to_string(item).unwrap_or_default());
                normalized.push(item.clone());
                continue;
            }
        };
        let mime_type = item["mimeType"]
            .as_str()
            .or_else(|| item["mime_type"].as_str())
            .unwrap_or("application/octet-stream");

        // Metadata-only alias of this block; the raw payload (if any) lives in
        // the typed collection below, not here, so it is never duplicated.
        let mut block = serde_json::Map::new();
        block.insert("type".into(), Value::String(kind.to_string()));
        block.insert("mimeType".into(), Value::String(mime_type.to_string()));

        match kind {
            "text" => {
                if let Some(t) = item["text"].as_str() {
                    text_parts.push(t.to_string());
                }
                block.insert(
                    "text".into(),
                    item.get("text")
                        .cloned()
                        .unwrap_or(Value::String(String::new())),
                );
            }
            "image" | "audio" => {
                let data = item["data"].as_str().unwrap_or("");
                let base64_bytes = data.len();
                let entry = if base64_bytes <= max_binary_base64_bytes {
                    serde_json::json!({
                        "type": kind,
                        "mimeType": mime_type,
                        "data": data,
                        "base64_bytes": base64_bytes,
                    })
                } else {
                    // Oversized payload: keep only metadata so the observation
                    // and DB record stay bounded.
                    serde_json::json!({
                        "type": kind,
                        "mimeType": mime_type,
                        "data": "",
                        "oversized": true,
                        "base64_bytes": base64_bytes,
                    })
                };
                if kind == "image" {
                    images.push(entry);
                } else {
                    audio.push(entry);
                }
                block.insert("base64_bytes".into(), Value::from(base64_bytes));
                // Marker so the text-only agent loop knows binary content exists.
                text_parts.push(format!(
                    "[{} block returned: {} ({} base64 chars{})]",
                    kind,
                    mime_type,
                    data.len(),
                    if base64_bytes > max_binary_base64_bytes {
                        ", oversized"
                    } else {
                        ""
                    }
                ));
            }
            "resource" => {
                let res = &item["resource"];
                if let Some(t) = res["text"].as_str() {
                    text_parts.push(t.to_string());
                    block.insert("text".into(), Value::String(t.to_string()));
                } else if let Some(blob) = res["blob"].as_str() {
                    let uri = res["uri"].as_str().unwrap_or("");
                    // Compute the decoded size without allocating the buffer.
                    let decoded_len = base64_decoded_len(blob);
                    let base64_bytes = blob.len();
                    let entry = if base64_bytes <= max_binary_base64_bytes {
                        serde_json::json!({
                            "uri": uri,
                            "mimeType": res["mimeType"].as_str().unwrap_or(mime_type),
                            "blob": blob,
                            "bytes": decoded_len,
                            "base64_bytes": base64_bytes,
                        })
                    } else {
                        serde_json::json!({
                            "uri": uri,
                            "mimeType": res["mimeType"].as_str().unwrap_or(mime_type),
                            "blob": "",
                            "oversized": true,
                            "bytes": decoded_len,
                            "base64_bytes": base64_bytes,
                        })
                    };
                    resources.push(entry);
                    block.insert("bytes".into(), Value::from(decoded_len));
                    block.insert("base64_bytes".into(), Value::from(base64_bytes));
                    text_parts.push(format!(
                        "[resource block returned: {} ({} base64 chars, ~{} decoded bytes{})]",
                        if uri.is_empty() { mime_type } else { uri },
                        blob.len(),
                        decoded_len,
                        if base64_bytes > max_binary_base64_bytes {
                            ", oversized"
                        } else {
                            ""
                        }
                    ));
                } else {
                    // Neither a readable text nor a blob: surface a marker so
                    // the block is not silently dropped (mirrors image/audio).
                    text_parts.push(format!(
                        "[resource block returned: {} (no readable payload)]",
                        res["uri"].as_str().unwrap_or(mime_type)
                    ));
                }
            }
            _ => {
                // Unknown block type — preserve it but don't fail.
                text_parts.push(serde_json::to_string(item).unwrap_or_default());
                normalized.push(item.clone());
                continue;
            }
        }

        normalized.push(Value::Object(block));
    }

    let text = text_parts.join("\n");
    let mut output = serde_json::Map::new();
    output.insert("text".into(), Value::String(text.clone()));
    output.insert("content".into(), Value::Array(normalized));
    if !images.is_empty() {
        output.insert("images".into(), Value::Array(images));
    }
    if !audio.is_empty() {
        output.insert("audio".into(), Value::Array(audio));
    }
    if !resources.is_empty() {
        output.insert("resources".into(), Value::Array(resources));
    }

    McpContentExtraction {
        output: Value::Object(output),
        text_summary: text,
    }
}

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct McpToolInfo {
    pub name: String,
    /// MCP marks description optional; missing → empty string.
    #[serde(default)]
    pub description: String,
    /// MCP wire key is camelCase `inputSchema`; Haven/UI keep snake_case on
    /// serialize. The field is required by the current MCP tools/list
    /// contract; malformed tool definitions are rejected at the discovery
    /// boundary instead of becoming an invalid null schema downstream.
    #[serde(alias = "inputSchema")]
    pub input_schema: Value,
}

#[derive(Debug, Clone, serde::Serialize)]
pub enum McpClientStatus {
    Disconnected,
    Connecting,
    Connected,
    Offline { error: String },
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct McpServerSnapshot {
    pub name: String,
    pub transport: McpServerTransport,
    pub stdio_execution: McpStdioExecution,
    pub command: String,
    pub args: Vec<String>,
    pub env: Vec<String>,
    pub cwd: Option<String>,
    pub url: String,
    pub enabled: bool,
    /// Opaque identity for the exact live manager connection represented by
    /// this snapshot. UI initiated reads pass it back to reject stale targets.
    pub target_binding: Option<String>,
    /// Negotiated server capabilities used to enable explicit host-side UI.
    pub supports_resources: bool,
    pub supports_prompts: bool,
    /// Haven-side model exposure policy for this configured server.
    pub exposure: McpExposureConfig,
    pub status: McpClientStatus,
    pub tools: Vec<McpToolInfo>,
    pub last_error: Option<String>,
    /// Protocol-selection/tool-discovery diagnostics (protocol mismatch,
    /// connected-but-zero-tools, failed list_tools). Lets the UI and the
    /// agent distinguish "the server has no tools" from "the client and the
    /// server are incompatible".
    pub diagnostic: Option<String>,
    pub last_seen_at: Option<i64>,
}

/// Explain why a configured server without a live client has no tool schema
/// snapshot. The count is unknown until a connection completes tools/list.
pub const MCP_TOOLS_NOT_DISCOVERED_DIAGNOSTIC: &str = "Not connected; the MCP tool list has not been discovered, so the tool count is unknown. Connect or load the server to inspect its tools.";

#[derive(Debug, Clone, serde::Serialize)]
pub struct McpStatusChangeEvent {
    pub name: String,
    pub status: McpClientStatus,
}

// ---------------------------------------------------------------------------
// McpClient — single MCP server connection (stdio or Streamable HTTP)
// ---------------------------------------------------------------------------
