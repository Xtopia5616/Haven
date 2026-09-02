use serde_json::Value;

pub(crate) const PROTOCOL_VERSION: &str = "2024-11-05";
pub(crate) const REQUEST_TIMEOUT_SECS: u64 = 30;

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
/// `max_binary_payload` base64 chars) for downstream rendering, while
/// `output.content` keeps a metadata-only list of every block (no raw payload)
/// for UI fidelity. Unknown or malformed block types are preserved rather than
/// silently dropped.
pub(crate) fn extract_mcp_content(content: &[Value], max_binary_payload: usize) -> (Value, String) {
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
                let entry = if data.len() <= max_binary_payload {
                    serde_json::json!({
                        "type": kind,
                        "mimeType": mime_type,
                        "data": data,
                    })
                } else {
                    // Oversized payload: keep only metadata so the observation
                    // and DB record stay bounded.
                    serde_json::json!({
                        "type": kind,
                        "mimeType": mime_type,
                        "data": "",
                        "oversized": true,
                        "bytes": data.len(),
                    })
                };
                if kind == "image" {
                    images.push(entry);
                } else {
                    audio.push(entry);
                }
                block.insert("data_len".into(), Value::from(data.len()));
                // Marker so the text-only agent loop knows binary content exists.
                text_parts.push(format!(
                    "[{} block returned: {} ({} base64 chars{})]",
                    kind,
                    mime_type,
                    data.len(),
                    if data.len() > max_binary_payload {
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
                    let entry = if blob.len() <= max_binary_payload {
                        serde_json::json!({
                            "uri": uri,
                            "mimeType": res["mimeType"].as_str().unwrap_or(mime_type),
                            "blob": blob,
                            "bytes": decoded_len,
                        })
                    } else {
                        serde_json::json!({
                            "uri": uri,
                            "mimeType": res["mimeType"].as_str().unwrap_or(mime_type),
                            "blob": "",
                            "oversized": true,
                            "bytes": decoded_len,
                        })
                    };
                    resources.push(entry);
                    block.insert("bytes".into(), Value::from(decoded_len));
                    text_parts.push(format!(
                        "[resource block returned: {} ({} base64 chars, ~{} decoded bytes{})]",
                        if uri.is_empty() { mime_type } else { uri },
                        blob.len(),
                        decoded_len,
                        if blob.len() > max_binary_payload {
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

    (Value::Object(output), text)
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
    /// serialize. Without the alias, serde ignores `inputSchema` and
    /// `#[serde(default)]` yields `Null`, which OpenAI Responses rejects
    /// (`null is not of types "boolean", "object"` at schema root).
    #[serde(default, alias = "inputSchema")]
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
    pub transport: String,
    pub command: String,
    pub args: Vec<String>,
    pub env: Vec<String>,
    pub cwd: Option<String>,
    pub url: String,
    pub enabled: bool,
    pub status: McpClientStatus,
    pub tools: Vec<McpToolInfo>,
    pub last_error: Option<String>,
    /// Handshake/tool-discovery diagnostics (protocol version mismatch,
    /// connected-but-zero-tools, failed list_tools). Lets the UI and the
    /// agent distinguish "the server has no tools" from "the client and the
    /// server are incompatible".
    pub diagnostic: Option<String>,
    pub last_seen_at: Option<i64>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct McpStatusChangeEvent {
    pub name: String,
    pub status: McpClientStatus,
}

// ---------------------------------------------------------------------------
// McpClient — single MCP server connection (stdio or Streamable HTTP)
// ---------------------------------------------------------------------------
