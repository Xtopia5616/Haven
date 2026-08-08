use async_trait::async_trait;
use haven_common::types::RiskLevel;
use serde_json::Value;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio_util::sync::CancellationToken;

use crate::{Tool, ToolResult};

/// One clipboard history entry.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ClipboardEntry {
    pub content: String,
    pub timestamp_ms: u64,
}

/// In-memory clipboard history shared across clipboard tool instances
/// (survives catalog rebuilds). Newest entries first.
pub struct ClipboardHistory {
    entries: Mutex<VecDeque<ClipboardEntry>>,
    max_entries: usize,
}

impl ClipboardHistory {
    pub fn new(max_entries: usize) -> Self {
        Self {
            entries: Mutex::new(VecDeque::new()),
            max_entries: max_entries.max(1),
        }
    }

    /// Record a copied/read text. Re-recording an existing entry moves it to
    /// the front (bumps its timestamp) instead of duplicating it.
    pub fn record(&self, content: String) {
        if content.is_empty() {
            return;
        }
        let timestamp_ms = now_ms();
        let mut entries = self.entries.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(idx) = entries.iter().position(|e| e.content == content) {
            let mut entry = entries.remove(idx).unwrap();
            entry.timestamp_ms = timestamp_ms;
            entries.push_front(entry);
        } else {
            entries.push_front(ClipboardEntry {
                content,
                timestamp_ms,
            });
        }
        while entries.len() > self.max_entries {
            entries.pop_back();
        }
    }

    /// Recent entries, newest first, capped at `limit`.
    pub fn recent(&self, limit: usize) -> Vec<ClipboardEntry> {
        let entries = self.entries.lock().unwrap_or_else(|p| p.into_inner());
        entries.iter().take(limit).cloned().collect()
    }

    pub fn len(&self) -> usize {
        self.entries.lock().unwrap_or_else(|p| p.into_inner()).len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Per-entry content truncation so a history dump stays readable.
pub struct ClipboardTool {
    history: Arc<ClipboardHistory>,
    /// Output cap (chars) for clipboard content.
    max_output_chars: usize,
    /// Default `limit` for the `history` operation when the caller omits it.
    default_limit: usize,
    /// Upper clamp for the `history` operation's `limit` argument.
    max_history_limit: usize,
    /// Per-entry content truncation for history dumps.
    entry_max_chars: usize,
}

impl ClipboardTool {
    pub fn new(
        history: Arc<ClipboardHistory>,
        max_output_chars: usize,
        default_limit: usize,
        max_history_limit: usize,
        entry_max_chars: usize,
    ) -> Self {
        Self {
            history,
            max_output_chars,
            default_limit,
            max_history_limit,
            entry_max_chars,
        }
    }
}

#[async_trait]
impl Tool for ClipboardTool {
    fn name(&self) -> String {
        "clipboard".into()
    }
    fn description(&self) -> String {
        "Read, write, or inspect the system clipboard history".into()
    }

    fn risk_level(&self, input: &Value) -> RiskLevel {
        match input["operation"].as_str() {
            Some("write") => RiskLevel::Medium,
            _ => RiskLevel::Low,
        }
    }

    fn input_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "operation": { "type": "string", "enum": ["read", "write", "history"] },
                "content": { "type": "string" },
                "limit": { "type": "integer", "minimum": 1, "maximum": 100 }
            },
            "required": ["operation"]
        })
    }

    async fn execute(&self, input: Value, cancel: CancellationToken) -> anyhow::Result<ToolResult> {
        let op = input["operation"].as_str().unwrap_or("read");

        if cancel.is_cancelled() {
            anyhow::bail!("cancelled");
        }

        match op {
            "read" => {
                let text = tokio::task::spawn_blocking(|| -> anyhow::Result<String> {
                    let mut cb = arboard::Clipboard::new()?;
                    cb.get_text()
                        .map_err(|e| anyhow::anyhow!("clipboard read failed: {}", e))
                })
                .await??;

                if cancel.is_cancelled() {
                    anyhow::bail!("cancelled");
                }
                self.history.record(text.clone());
                let max_chars = self.max_output_chars;
                let (text, truncated) = haven_common::encoding::truncate_output(&text, max_chars);
                let mut result = serde_json::json!({"content": text});
                if truncated {
                    result["truncated"] = serde_json::Value::Bool(true);
                }
                Ok(ToolResult::ok(result))
            }
            "write" => {
                let content = input["content"]
                    .as_str()
                    .ok_or_else(|| anyhow::anyhow!("'content' is required for write operation"))?
                    .to_string();

                tokio::task::spawn_blocking({
                    let content = content.clone();
                    move || -> anyhow::Result<()> {
                        let mut cb = arboard::Clipboard::new()?;
                        cb.set_text(content)
                            .map_err(|e| anyhow::anyhow!("clipboard write failed: {}", e))
                    }
                })
                .await??;

                if cancel.is_cancelled() {
                    anyhow::bail!("cancelled");
                }
                self.history.record(content);
                Ok(ToolResult::ok(serde_json::json!({"written": true})))
            }
            "history" => {
                let limit = input["limit"]
                    .as_u64()
                    .map(|l| (l.min(self.max_history_limit as u64)) as usize)
                    .unwrap_or(self.default_limit);
                let entries = self.history.recent(limit);
                let json_entries: Vec<Value> = entries
                    .iter()
                    .map(|e| {
                        let (content, _) = haven_common::encoding::truncate_output(
                            &e.content,
                            self.entry_max_chars,
                        );
                        serde_json::json!({
                            "content": content,
                            "timestamp_ms": e.timestamp_ms,
                        })
                    })
                    .collect();
                Ok(ToolResult::ok(serde_json::json!({
                    "entries": json_entries,
                    "total": self.history.len(),
                })))
            }
            _ => anyhow::bail!("unknown clipboard operation: {}", op),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Tool;
    use serde_json::json;

    fn test_tool() -> ClipboardTool {
        ClipboardTool::new(Arc::new(ClipboardHistory::new(10)), 20_000, 10, 100, 2000)
    }

    #[test]
    fn test_clipboard_tool_name() {
        assert_eq!(test_tool().name(), "clipboard");
    }

    #[test]
    fn test_clipboard_tool_description() {
        assert!(test_tool().description().contains("clipboard"));
    }

    #[test]
    fn test_clipboard_tool_risk_level() {
        assert_eq!(
            test_tool().risk_level(&json!({"operation": "write"})),
            RiskLevel::Medium
        );
        assert_eq!(
            test_tool().risk_level(&json!({"operation": "read"})),
            RiskLevel::Low
        );
        assert_eq!(
            test_tool().risk_level(&json!({"operation": "history"})),
            RiskLevel::Low
        );
    }

    #[test]
    fn test_clipboard_tool_input_schema() {
        let schema = test_tool().input_schema();
        assert_eq!(schema["type"].as_str().unwrap(), "object");
        let enum_vals = schema["properties"]["operation"]["enum"]
            .as_array()
            .unwrap();
        let ops: Vec<&str> = enum_vals.iter().map(|v| v.as_str().unwrap()).collect();
        assert!(ops.contains(&"read"));
        assert!(ops.contains(&"write"));
        assert!(ops.contains(&"history"));
    }

    #[tokio::test]
    async fn test_clipboard_write_read_roundtrip() {
        let content = format!("haven-clipboard-test-{}", std::process::id());
        let write = test_tool()
            .execute(
                json!({"operation": "write", "content": content.clone()}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(write.success);
        assert_eq!(write.output["written"], true);

        let read = test_tool()
            .execute(json!({"operation": "read"}), CancellationToken::new())
            .await
            .unwrap();
        assert!(read.success);
        assert_eq!(read.output["content"], content);
    }

    #[tokio::test]
    async fn test_clipboard_write_requires_content() {
        let result = test_tool()
            .execute(json!({"operation": "write"}), CancellationToken::new())
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_clipboard_unknown_operation() {
        let result = test_tool()
            .execute(json!({"operation": "bogus"}), CancellationToken::new())
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_clipboard_execute_cancelled() {
        let cancel = CancellationToken::new();
        cancel.cancel();
        let result = test_tool()
            .execute(json!({"operation": "read"}), cancel)
            .await;
        assert!(result.is_err());
    }

    #[test]
    fn test_history_records_newest_first() {
        let history = ClipboardHistory::new(10);
        assert!(history.is_empty());
        history.record("first".into());
        history.record("second".into());
        let recent = history.recent(10);
        assert_eq!(recent.len(), 2);
        assert_eq!(recent[0].content, "second");
        assert_eq!(recent[1].content, "first");
    }

    #[test]
    fn test_history_dedupes_most_recent() {
        let history = ClipboardHistory::new(10);
        history.record("a".into());
        history.record("b".into());
        history.record("a".into());
        let recent = history.recent(10);
        assert_eq!(recent.len(), 2, "re-copying 'a' must not duplicate it");
        assert_eq!(recent[0].content, "a");
        assert_eq!(recent[1].content, "b");
    }

    #[test]
    fn test_history_caps_entries() {
        let history = ClipboardHistory::new(3);
        for i in 0..10 {
            history.record(format!("item-{}", i));
        }
        let recent = history.recent(100);
        assert_eq!(recent.len(), 3);
        assert_eq!(recent[0].content, "item-9");
        assert_eq!(recent[2].content, "item-7");
    }

    #[test]
    fn test_history_ignores_empty() {
        let history = ClipboardHistory::new(10);
        history.record(String::new());
        assert!(history.is_empty());
    }

    #[tokio::test]
    async fn test_history_operation_returns_recorded_entries() {
        let tool = test_tool();
        tool.history.record("alpha".into());
        tool.history.record("beta".into());

        let result = tool
            .execute(json!({"operation": "history"}), CancellationToken::new())
            .await
            .unwrap();
        assert!(result.success);
        let entries = result.output["entries"].as_array().unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0]["content"], "beta");
        assert_eq!(entries[1]["content"], "alpha");
        assert_eq!(result.output["total"], 2);
        assert!(entries[0]["timestamp_ms"].as_u64().unwrap() > 0);
    }

    #[tokio::test]
    async fn test_history_operation_respects_limit() {
        let tool = test_tool();
        for i in 0..5 {
            tool.history.record(format!("item-{}", i));
        }
        let result = tool
            .execute(
                json!({"operation": "history", "limit": 2}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        let entries = result.output["entries"].as_array().unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0]["content"], "item-4");
        assert_eq!(result.output["total"], 5);
    }
}
