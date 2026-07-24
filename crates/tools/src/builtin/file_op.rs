use async_trait::async_trait;
use haven_common::types::RiskLevel;
use serde_json::Value;
use std::path::Path;
use tokio_util::sync::CancellationToken;

use crate::{Tool, ToolResult};

fn sanitize_path(path: &str) -> anyhow::Result<String> {
    let normalized = Path::new(path)
        .components()
        .collect::<std::path::PathBuf>();
    let s = normalized.to_string_lossy().to_string();
    if s.contains("..") {
        anyhow::bail!("path traversal detected: '{}'", path);
    }
    Ok(s)
}

fn truncate_output(text: &str, max_chars: usize) -> (String, bool) {
    if text.len() <= max_chars {
        (text.to_string(), false)
    } else {
        let truncated = format!(
            "{}[truncated ... {} chars omitted]",
            &text[..max_chars],
            text.len() - max_chars
        );
        (truncated, true)
    }
}

pub struct FileOpTool;

#[async_trait]
impl Tool for FileOpTool {
    fn name(&self) -> String {
        "file".into()
    }
    fn description(&self) -> String {
        "File operations: read, write, copy, move, delete, list".into()
    }

    fn risk_level(&self, input: &Value) -> RiskLevel {
        match input["operation"].as_str() {
            Some("delete") => RiskLevel::High,
            Some("copy") | Some("write") | Some("move") => RiskLevel::Medium,
            _ => RiskLevel::Low,
        }
    }

    fn input_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "operation": { "type": "string", "enum": ["read", "write", "copy", "move", "delete", "list"] },
                "path": { "type": "string" },
                "destination": { "type": "string" },
                "content": { "type": "string" }
            },
            "required": ["operation", "path"]
        })
    }

    async fn execute(
        &self,
        input: Value,
        cancel: CancellationToken,
    ) -> anyhow::Result<ToolResult> {
        let op = input["operation"].as_str().unwrap_or("read");
        let path = sanitize_path(input["path"].as_str().unwrap_or(""))?;
        let max_chars = self.max_output_chars();

        if cancel.is_cancelled() {
            anyhow::bail!("cancelled");
        }

        match op {
            "read" => {
                let content = tokio::fs::read_to_string(&path).await?;
                let (output, truncated) = truncate_output(&content, max_chars);
                if truncated {
                    Ok(ToolResult::truncated(serde_json::json!({"content": output})))
                } else {
                    Ok(ToolResult::ok(serde_json::json!({"content": output})))
                }
            }
            "write" => {
                let content = input["content"].as_str().unwrap_or("");
                tokio::fs::write(&path, content).await?;
                if cancel.is_cancelled() {
                    anyhow::bail!("cancelled");
                }
                Ok(ToolResult::ok(
                    serde_json::json!({"written": true, "path": path}),
                ))
            }
            "copy" => {
                let dest = sanitize_path(input["destination"].as_str().unwrap_or(""))?;
                tokio::fs::copy(&path, &dest).await?;
                if cancel.is_cancelled() {
                    anyhow::bail!("cancelled");
                }
                Ok(ToolResult::ok(
                    serde_json::json!({"copied": true, "from": path, "to": dest}),
                ))
            }
            "move" => {
                let dest = sanitize_path(input["destination"].as_str().unwrap_or(""))?;
                tokio::fs::rename(&path, &dest).await?;
                if cancel.is_cancelled() { anyhow::bail!("cancelled"); }
                Ok(ToolResult::ok(
                    serde_json::json!({"moved": true, "from": path, "to": dest}),
                ))
            }
            "delete" => {
                tokio::fs::remove_file(&path).await?;
                if cancel.is_cancelled() { anyhow::bail!("cancelled"); }
                Ok(ToolResult::ok(
                    serde_json::json!({"deleted": true, "path": path}),
                ))
            }
            "list" => {
                let mut entries = tokio::fs::read_dir(&path).await?;
                let mut names = Vec::new();
                while let Some(entry) = entries.next_entry().await? {
                    if cancel.is_cancelled() { anyhow::bail!("cancelled"); }
                    names.push(entry.file_name().to_string_lossy().to_string());
                }
                Ok(ToolResult::ok(serde_json::json!({"entries": names})))
            }
            _ => anyhow::bail!("unknown file operation: {}", op),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use haven_common::types::RiskLevel;
    use serde_json::json;
    use tempfile::TempDir;

    #[test]
    fn test_sanitize_path_normal() {
        let result = sanitize_path("file.txt");
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "file.txt");
    }

    #[test]
    fn test_sanitize_path_traversal() {
        let tmp = TempDir::new().unwrap();
        let dotted = tmp.path().join("..").join("file.txt");
        let path_str = dotted.to_string_lossy().to_string();
        let result = sanitize_path(&path_str);
        assert!(result.is_err());
    }

    #[test]
    fn test_sanitize_path_relative() {
        let result = sanitize_path("relative/path/file.txt");
        assert!(result.is_ok());
        assert!(!result.unwrap().contains(".."));
    }

    #[test]
    fn test_truncate_output_short() {
        let (out, truncated) = truncate_output("hello", 100);
        assert_eq!(out, "hello");
        assert!(!truncated);
    }

    #[test]
    fn test_truncate_output_long() {
        let text = "a".repeat(200);
        let (out, truncated) = truncate_output(&text, 100);
        assert!(truncated);
        assert!(out.len() < text.len());
        assert!(out.contains("[truncated ... 100 chars omitted]"));
    }

    #[test]
    fn test_file_op_name() {
        assert_eq!(FileOpTool.name(), "file");
    }

    #[test]
    fn test_file_op_description() {
        assert!(FileOpTool.description().contains("File operations"));
    }

    #[test]
    fn test_file_op_risk_level() {
        assert_eq!(FileOpTool.risk_level(&json!({"operation": "delete"})), RiskLevel::High);
        assert_eq!(FileOpTool.risk_level(&json!({"operation": "write"})), RiskLevel::Medium);
        assert_eq!(FileOpTool.risk_level(&json!({"operation": "move"})), RiskLevel::Medium);
        assert_eq!(FileOpTool.risk_level(&json!({"operation": "copy"})), RiskLevel::Medium);
        assert_eq!(FileOpTool.risk_level(&json!({"operation": "read"})), RiskLevel::Low);
        assert_eq!(FileOpTool.risk_level(&json!({"operation": "list"})), RiskLevel::Low);
    }

    #[test]
    fn test_file_op_input_schema() {
        let schema = FileOpTool.input_schema();
        assert_eq!(schema["type"].as_str().unwrap(), "object");
        let required = schema["required"].as_array().unwrap();
        let req: Vec<&str> = required.iter().map(|v| v.as_str().unwrap()).collect();
        assert!(req.contains(&"operation"));
        assert!(req.contains(&"path"));
        let enum_vals = schema["properties"]["operation"]["enum"].as_array().unwrap();
        let ops: Vec<&str> = enum_vals.iter().map(|v| v.as_str().unwrap()).collect();
        assert!(ops.contains(&"read"));
        assert!(ops.contains(&"write"));
        assert!(ops.contains(&"copy"));
        assert!(ops.contains(&"move"));
        assert!(ops.contains(&"delete"));
        assert!(ops.contains(&"list"));
    }

    #[tokio::test]
    async fn test_file_op_execute_read() {
        let tmp = TempDir::new().unwrap();
        let file = tmp.path().join("readme.txt");
        tokio::fs::write(&file, "hello world").await.unwrap();
        let path_str = file.to_string_lossy().to_string();

        let result = FileOpTool
            .execute(json!({"operation": "read", "path": path_str}), CancellationToken::new())
            .await
            .unwrap();
        assert!(result.success);
        assert_eq!(result.output["content"].as_str().unwrap(), "hello world");
    }

    #[tokio::test]
    async fn test_file_op_execute_write() {
        let tmp = TempDir::new().unwrap();
        let file = tmp.path().join("output.txt");
        let path_str = file.to_string_lossy().to_string();

        let result = FileOpTool
            .execute(
                json!({"operation": "write", "path": path_str, "content": "written content"}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(result.success);
        assert!(result.output["written"].as_bool().unwrap());
        let content = tokio::fs::read_to_string(&file).await.unwrap();
        assert_eq!(content, "written content");
    }

    #[tokio::test]
    async fn test_file_op_execute_copy() {
        let tmp = TempDir::new().unwrap();
        let src = tmp.path().join("source.txt");
        let dst = tmp.path().join("dest.txt");
        tokio::fs::write(&src, "copy me").await.unwrap();

        let result = FileOpTool
            .execute(
                json!({
                    "operation": "copy",
                    "path": src.to_string_lossy(),
                    "destination": dst.to_string_lossy()
                }),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(result.success);
        assert!(src.exists());
        assert!(dst.exists());
        let content = tokio::fs::read_to_string(&dst).await.unwrap();
        assert_eq!(content, "copy me");
    }

    #[tokio::test]
    async fn test_file_op_execute_move() {
        let tmp = TempDir::new().unwrap();
        let src = tmp.path().join("source.txt");
        let dst = tmp.path().join("target.txt");
        tokio::fs::write(&src, "move me").await.unwrap();

        let result = FileOpTool
            .execute(
                json!({
                    "operation": "move",
                    "path": src.to_string_lossy(),
                    "destination": dst.to_string_lossy()
                }),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(result.success);
        assert!(!src.exists());
        assert!(dst.exists());
    }

    #[tokio::test]
    async fn test_file_op_execute_delete() {
        let tmp = TempDir::new().unwrap();
        let file = tmp.path().join("to_delete.txt");
        tokio::fs::write(&file, "delete me").await.unwrap();
        assert!(file.exists());

        let result = FileOpTool
            .execute(
                json!({"operation": "delete", "path": file.to_string_lossy()}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(result.success);
        assert!(result.output["deleted"].as_bool().unwrap());
        assert!(!file.exists());
    }

    #[tokio::test]
    async fn test_file_op_execute_list() {
        let tmp = TempDir::new().unwrap();
        tokio::fs::write(tmp.path().join("a.txt"), "a").await.unwrap();
        tokio::fs::write(tmp.path().join("b.txt"), "b").await.unwrap();

        let result = FileOpTool
            .execute(
                json!({"operation": "list", "path": tmp.path().to_string_lossy()}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(result.success);
        let entries = result.output["entries"].as_array().unwrap();
        let names: Vec<&str> = entries.iter().map(|v| v.as_str().unwrap()).collect();
        assert!(names.contains(&"a.txt"));
        assert!(names.contains(&"b.txt"));
    }

    #[tokio::test]
    async fn test_file_op_execute_unknown() {
        let result = FileOpTool
            .execute(
                json!({"operation": "unknown", "path": "file.txt"}),
                CancellationToken::new(),
            )
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_file_op_execute_cancelled() {
        let cancel = CancellationToken::new();
        cancel.cancel();
        let result = FileOpTool
            .execute(json!({"operation": "read", "path": "file.txt"}), cancel)
            .await;
        assert!(result.is_err());
    }
}
