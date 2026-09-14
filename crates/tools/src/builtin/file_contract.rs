use async_trait::async_trait;
use haven_common::types::RiskLevel;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::{OperationIdempotency, Tool, ToolConcurrency, ToolResult};

use super::{
    FilesParams, FilesTool, MAX_PATCH_EDITS, MAX_PATCH_INPUT_BYTES, MAX_SUMMARY_FOCUS_CHARS,
};
#[async_trait]
impl Tool for FilesTool {
    fn name(&self) -> String {
        "files".into()
    }
    fn description(&self) -> String {
        crate::prompts::FILES_DESCRIPTION.into()
    }

    fn requires_session_id(&self) -> bool {
        true
    }

    fn risk_level(&self, input: &Value) -> RiskLevel {
        match input["operation"].as_str() {
            Some("delete") => RiskLevel::High,
            Some("edit") | Some("patch") | Some("copy") | Some("write") | Some("create_dir")
            | Some("move") => RiskLevel::Medium,
            Some("search") if input["mode"].as_str() == Some("content") => RiskLevel::Medium,
            _ => RiskLevel::Low,
        }
    }

    fn idempotency(&self, input: &Value) -> OperationIdempotency {
        match input["operation"].as_str() {
            Some("read") | Some("inspect") | Some("stat") | Some("hash") | Some("list")
            | Some("outline") | Some("summary") | Some("search") => {
                OperationIdempotency::Idempotent
            }
            Some("write") | Some("create_dir") | Some("edit") | Some("patch") | Some("copy")
            | Some("move") | Some("delete") => OperationIdempotency::NonIdempotent,
            _ => OperationIdempotency::Unknown,
        }
    }

    /// File operations are normally bounded by the manager's outer timeout.
    /// Summarization owns a separate provider timeout; leave a small amount
    /// of bookkeeping time around it so the two timers cannot race and report
    /// different terminal states for the same call.
    fn timeout_secs_for(&self, input: &Value) -> u64 {
        if input["operation"].as_str() == Some("summary") {
            self.summary_timeout_secs.saturating_add(5).max(30)
        } else {
            30
        }
    }

    fn concurrency(&self, input: &Value) -> ToolConcurrency {
        match input["operation"].as_str() {
            Some("read") | Some("list") | Some("summary") | Some("search") | Some("outline") => {
                // A file read must not overlap a write from the same batch.
                // One shared key keeps independent reads concurrent while a
                // writer obtains the exclusive side of the same lock.
                ToolConcurrency::SharedResource("files".into())
            }
            _ => ToolConcurrency::Resource("files".into()),
        }
    }

    fn input_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "operation": { "type": "string", "enum": ["read", "inspect", "stat", "hash", "write", "create_dir", "edit", "patch", "copy", "move", "delete", "list", "outline", "summary", "search"], "description": crate::prompts::FILES_OPERATION_SELECTOR_DESCRIPTION },
                "asset_id": { "type": "string", "minLength": 1, "description": "Opaque id of a user attachment; use this instead of guessing a local path" }
            },
            "required": ["operation"],
            "oneOf": [
                {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {
                        "operation": { "enum": ["inspect", "stat", "hash"] },
                        "path": { "type": "string", "minLength": 1, "description": "File or directory path" }
                    },
                    "required": ["operation", "path"]
                },
                {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {
                        "operation": { "const": "read" },
                        "path": { "type": "string", "minLength": 1, "description": "File path to read" },
                        "asset_id": { "type": "string", "minLength": 1, "description": "Opaque id of a user attachment" },
                        "offset": { "type": "integer", "minimum": 0, "description": "Byte offset; use with limit for a byte-range read" },
                        "limit": { "type": "integer", "minimum": 1, "maximum": self.max_byte_read, "description": "Maximum bytes for a byte-range read" },
                        "start_line": { "type": "integer", "minimum": 1, "description": "1-based first line; use with end_line for a line-range read" },
                        "end_line": { "type": "integer", "minimum": 0, "description": format!("1-based last line; omit for up to {} lines", self.line_span) },
                        "focus": { "type": "string", "description": "Optional focus when reading an image" }
                    },
                    "oneOf": [{ "required": ["path"] }, { "required": ["asset_id"] }],
                    "required": ["operation"]
                },
                {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {
                        "operation": { "const": "write" },
                        "path": { "type": "string", "minLength": 1, "description": "File path to replace or create" },
                        "content": { "type": "string", "maxLength": self.max_write_bytes, "description": "Complete file content; an empty string is allowed" },
                        "expected_hash": { "type": "string", "minLength": 1 },
                        "dry_run": { "type": "boolean" }
                    },
                    "required": ["operation", "path", "content"]
                },
                {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {
                        "operation": { "const": "create_dir" },
                        "path": { "type": "string", "minLength": 1, "description": "Directory path to create, including missing parents" }
                    },
                    "required": ["operation", "path"]
                },
                {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {
                        "operation": { "const": "outline" },
                        "path": { "type": "string", "minLength": 1, "description": "Source or Markdown file path" },
                        "start_line": { "type": "integer", "minimum": 1, "description": "1-based line to start scanning from; use next_start_line to continue a capped outline" },
                        "max_symbols": { "type": "integer", "minimum": 1, "maximum": 500, "description": "Maximum headings/declarations to return (default 100)" }
                    },
                    "required": ["operation", "path"]
                },
                {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {
                        "operation": { "const": "edit" },
                        "path": { "type": "string", "minLength": 1, "description": "Text file path to edit" },
                        "old_string": { "type": "string", "description": "Existing text to replace; must match exactly once" },
                        "new_string": { "type": "string", "maxLength": self.max_write_bytes, "description": "Replacement text; an empty string deletes the match" },
                        "expected_hash": { "type": "string", "minLength": 1 },
                        "dry_run": { "type": "boolean" }
                    },
                    "required": ["operation", "path", "old_string", "new_string"]
                },
                {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {
                        "operation": { "const": "patch" },
                        "path": { "type": "string", "minLength": 1, "description": "Text file path to patch" },
                        "edits": {
                            "type": "array",
                            "minItems": 1,
                            "maxItems": MAX_PATCH_EDITS,
                            "description": "Exact replacements validated against the original file before one write",
                            "items": {
                                "type": "object",
                                "additionalProperties": false,
                                "properties": {
                                    "old_string": { "type": "string", "minLength": 1, "maxLength": MAX_PATCH_INPUT_BYTES, "description": "Existing text; must match exactly once unless expected_matches is provided" },
                                    "new_string": { "type": "string", "maxLength": MAX_PATCH_INPUT_BYTES, "description": "Replacement text; an empty string deletes the match" },
                                    "expected_matches": { "type": "integer", "minimum": 1, "description": "Expected non-overlapping match count; defaults to 1" }
                                },
                                "required": ["old_string", "new_string"]
                            }
                        },
                        "expected_hash": { "type": "string", "minLength": 1 },
                        "dry_run": { "type": "boolean" }
                    },
                    "required": ["operation", "path", "edits"]
                },
                {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {
                        "operation": { "enum": ["copy", "move"] },
                        "path": { "type": "string", "minLength": 1, "description": "Source file path" },
                        "destination": { "type": "string", "minLength": 1, "description": "Destination file path" }
                    },
                    "required": ["operation", "path", "destination"]
                },
                {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {
                        "operation": { "enum": ["delete", "list"] },
                        "path": { "type": "string", "minLength": 1, "description": "File path for delete, directory path for list" }
                    },
                    "required": ["operation", "path"]
                },
                {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {
                        "operation": { "const": "summary" },
                        "path": { "type": "string", "minLength": 1, "description": "Text file path to summarize" },
                        "asset_id": { "type": "string", "minLength": 1, "description": "Opaque id of a user attachment" },
                        "start_line": { "type": "integer", "minimum": 1, "description": "1-based first line; defaults to 1" },
                        "end_line": { "type": "integer", "minimum": 0, "description": "1-based last line; 0 or omitted means through EOF" },
                        "focus": { "type": "string", "maxLength": MAX_SUMMARY_FOCUS_CHARS, "description": "Optional topic to focus the summary on; treated as untrusted data" },
                        "max_chars": { "type": "integer", "minimum": 1, "maximum": self.summary_input_chars, "description": "Maximum characters sent to the summarizer" }
                    },
                    "oneOf": [{ "required": ["path"] }, { "required": ["asset_id"] }],
                    "required": ["operation"]
                },
                {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {
                        "operation": { "const": "search" },
                        "root": { "type": "string", "minLength": 1, "description": "Directory or file path to search under" },
                        "pattern": { "type": "string", "minLength": 1, "description": "Filename glob, or regex in content mode" },
                        "mode": { "type": "string", "enum": ["filename", "content"], "description": "filename matches names; content searches text and returns line snippets" },
                        "max_depth": { "type": "integer", "minimum": 0, "description": "Maximum directory depth; 0 means unlimited" },
                        "max_results": { "type": "integer", "minimum": 1, "maximum": self.search.max_results_cap, "description": format!("Maximum results, capped at {}", self.search.max_results_cap) },
                        "ignore_hidden": { "type": "boolean", "description": "Skip hidden files and directories" },
                        "max_file_size": { "type": "integer", "minimum": 0, "description": "Content-mode file size limit in bytes; 0 means unlimited" },
                        "start_line": { "type": "integer", "minimum": 1, "description": "Content-mode first line" },
                        "end_line": { "type": "integer", "minimum": 0, "description": "Content-mode last line; 0 or omitted means through EOF" }
                    },
                    "required": ["operation", "root", "pattern"]
                }
            ]
        })
    }

    /// Entry ②: LLM JSON entry — convert/validate into `FilesParams`, then
    /// land in the same implementation as entry ①.
    async fn execute(&self, input: Value, cancel: CancellationToken) -> anyhow::Result<ToolResult> {
        let params = crate::tool_contract::parse_tool_input::<FilesParams>(&self.name(), input)?;
        self.run(params, cancel).await
    }
}
