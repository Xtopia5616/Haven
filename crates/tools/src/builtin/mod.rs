use async_trait::async_trait;
use haven_common::types::RiskLevel;
use haven_common::encoding;
use serde_json::Value;
use std::path::Path;
use std::sync::Arc;
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;

use crate::skills::SkillsEngine;
use crate::{Tool, ToolBox, ToolResult};
use crate::adapters::SkillToolAdapter;
use crate::skills::runner::SkillRunner;
use crate::ToolRegistry;

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

pub struct LoadSkillTool {
    pub skills_engine: SkillsEngine,
    pub registry: ToolRegistry,
    pub skill_runner: Arc<RwLock<SkillRunner>>,
}

#[async_trait]
impl Tool for LoadSkillTool {
    fn name(&self) -> String {
        "load_skill".into()
    }
    fn description(&self) -> String {
        "Load a skill's full tool schema by name. Use this when you need to access a skill listed in the Skill Index.".into()
    }

    fn risk_level(&self, _input: &Value) -> RiskLevel {
        RiskLevel::Safe
    }

    fn input_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "skill_name": { "type": "string", "description": "The name of the skill to load" }
            },
            "required": ["skill_name"]
        })
    }

    async fn execute(&self, input: Value, cancel: CancellationToken) -> anyhow::Result<ToolResult> {
        if cancel.is_cancelled() { anyhow::bail!("cancelled"); }
        let skill_name = input["skill_name"].as_str().unwrap_or("");
        if skill_name.is_empty() {
            anyhow::bail!("skill_name is required");
        }
        let skill = self
            .skills_engine
            .get_skill(skill_name)
            .await
            .ok_or_else(|| anyhow::anyhow!("skill '{}' not found", skill_name))?;
        let schema = serde_json::json!({
            "name": format!("skill::{}", skill.name()),
            "description": skill.description(),
            "input_schema": {
                "type": "object",
                "properties": {
                    "params": {
                        "type": "object",
                        "description": skill.instructions()
                    }
                },
                "required": ["params"]
            }
        });

        // Dynamically register the SkillToolAdapter so the LLM can call it
        let runner = self.skill_runner.read().await.clone();
        let adapter = SkillToolAdapter::new(Arc::new(skill), runner);
        self.registry.register(Arc::new(adapter)).await;

        Ok(ToolResult::ok(serde_json::json!({"skill": schema, "status": "loaded"})))
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
            Some("write") | Some("move") => RiskLevel::Medium,
            Some("copy") => RiskLevel::Low,
            _ => RiskLevel::Safe,
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

pub struct ProcessTool;

#[async_trait]
impl Tool for ProcessTool {
    fn name(&self) -> String {
        "process".into()
    }
    fn description(&self) -> String {
        "Process operations: list, launch, kill".into()
    }

    fn risk_level(&self, input: &Value) -> RiskLevel {
        match input["operation"].as_str() {
            Some("kill") => RiskLevel::Medium,
            _ => RiskLevel::Safe,
        }
    }

    fn input_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "operation": { "type": "string", "enum": ["list", "launch", "kill"] },
                "command": { "type": "string" },
                "pid": { "type": "integer" }
            },
            "required": ["operation"]
        })
    }

    async fn execute(
        &self,
        input: Value,
        cancel: CancellationToken,
    ) -> anyhow::Result<ToolResult> {
        let op = input["operation"].as_str().unwrap_or("list");
        let max_chars = self.max_output_chars();

        if cancel.is_cancelled() { anyhow::bail!("cancelled"); }

        match op {
            "list" => {
                let output = tokio::process::Command::new("tasklist").output().await?;
                if cancel.is_cancelled() { anyhow::bail!("cancelled"); }
                let text = encoding::decode_lossy(&output.stdout);
                let (output, truncated) = truncate_output(&text, max_chars);
                if truncated {
                    Ok(ToolResult::truncated(serde_json::json!({"processes": output})))
                } else {
                    Ok(ToolResult::ok(serde_json::json!({"processes": output})))
                }
            }
            "launch" => {
                let cmd = input["command"].as_str().unwrap_or("");
                tokio::process::Command::new("cmd")
                    .args(["/c", cmd])
                    .spawn()?;
                Ok(ToolResult::ok(serde_json::json!({"launched": cmd})))
            }
            "kill" => {
                let pid = input["pid"].as_i64().unwrap_or(0);
                tokio::process::Command::new("taskkill")
                    .args(["/PID", &pid.to_string(), "/F"])
                    .output()
                    .await?;
                if cancel.is_cancelled() { anyhow::bail!("cancelled"); }
                Ok(ToolResult::ok(serde_json::json!({"killed": pid})))
            }
            _ => anyhow::bail!("unknown process operation: {}", op),
        }
    }
}

pub struct ClipboardTool;

#[async_trait]
impl Tool for ClipboardTool {
    fn name(&self) -> String {
        "clipboard".into()
    }
    fn description(&self) -> String {
        "Clipboard read and write operations".into()
    }

    fn risk_level(&self, input: &Value) -> RiskLevel {
        match input["operation"].as_str() {
            Some("write") => RiskLevel::Low,
            _ => RiskLevel::Safe,
        }
    }

    fn input_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "operation": { "type": "string", "enum": ["read", "write"] },
                "content": { "type": "string" }
            },
            "required": ["operation"]
        })
    }

    async fn execute(
        &self,
        input: Value,
        cancel: CancellationToken,
    ) -> anyhow::Result<ToolResult> {
        let op = input["operation"].as_str().unwrap_or("read");
        let max_chars = self.max_output_chars();

        if cancel.is_cancelled() { anyhow::bail!("cancelled"); }

        match op {
            "read" => {
                let output = tokio::process::Command::new("powershell")
                    .args(["-Command", "Get-Clipboard"])
                    .output()
                    .await?;
                if cancel.is_cancelled() { anyhow::bail!("cancelled"); }
                if !output.status.success() {
                    let stderr = encoding::decode_lossy(&output.stderr);
                    anyhow::bail!("clipboard read failed: {}", stderr.trim());
                }
                let text = encoding::decode_lossy(&output.stdout).to_string();
                let (output, truncated) = truncate_output(&text, max_chars);
                if truncated {
                    Ok(ToolResult::truncated(serde_json::json!({"content": output})))
                } else {
                    Ok(ToolResult::ok(serde_json::json!({"content": output})))
                }
            }
            "write" => {
                let content = input["content"].as_str().ok_or_else(|| {
                    anyhow::anyhow!("'content' is required for write operation")
                })?;
                let mut child = tokio::process::Command::new("powershell")
                    .args(["-Command", "$input | Set-Clipboard"])
                    .stdin(std::process::Stdio::piped())
                    .stdout(std::process::Stdio::piped())
                    .stderr(std::process::Stdio::piped())
                    .spawn()?;
                if cancel.is_cancelled() { anyhow::bail!("cancelled"); }

                {
                    use tokio::io::AsyncWriteExt;
                    let mut stdin = child.stdin.take().ok_or_else(|| {
                        anyhow::anyhow!("failed to open stdin for powershell")
                    })?;
                    stdin.write_all(content.as_bytes()).await?;
                    stdin.flush().await?;
                }

                let output = child.wait_with_output().await?;
                if cancel.is_cancelled() { anyhow::bail!("cancelled"); }
                if !output.status.success() {
                    let stderr = encoding::decode_lossy(&output.stderr);
                    anyhow::bail!("clipboard write failed: {}", stderr.trim());
                }
                Ok(ToolResult::ok(serde_json::json!({"written": true})))
            }
            _ => anyhow::bail!("unknown clipboard operation: {}", op),
        }
    }
}

pub struct ShellTool;

#[async_trait]
impl Tool for ShellTool {
    fn name(&self) -> String {
        "shell".into()
    }
    fn description(&self) -> String {
        "Execute a shell command on the user's PC. When silent is true, the command output is hidden from the user but still returned to the agent.".into()
    }

    fn risk_level(&self, _input: &Value) -> RiskLevel {
        RiskLevel::High
    }

    fn input_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "command": { "type": "string", "description": "Shell command to execute" },
                "silent": { "type": "boolean", "description": "If true, hide output from the user (agent always sees it)", "default": false }
            },
            "required": ["command"]
        })
    }

    async fn execute(
        &self,
        input: Value,
        cancel: CancellationToken,
    ) -> anyhow::Result<ToolResult> {
        let cmd = input["command"].as_str().unwrap_or("");
        if cmd.is_empty() {
            anyhow::bail!("command is required");
        }
        let max_chars = self.max_output_chars();

        if cancel.is_cancelled() {
            anyhow::bail!("cancelled");
        }

        let output = tokio::process::Command::new("cmd")
            .args(["/C", cmd])
            .output()
            .await?;

        if cancel.is_cancelled() {
            anyhow::bail!("cancelled");
        }

        let stdout = encoding::decode_lossy(&output.stdout);
        let stderr = encoding::decode_lossy(&output.stderr);

        let mut combined = String::new();
        if !stdout.is_empty() {
            combined.push_str(&stdout);
        }
        if !stderr.is_empty() {
            if !combined.is_empty() {
                combined.push('\n');
            }
            combined.push_str(&stderr);
        }

        let (text, _truncated) = truncate_output(&combined, max_chars);
        if output.status.success() {
            Ok(ToolResult::ok(serde_json::json!({"output": text})))
        } else {
            Ok(ToolResult {
                success: false,
                output: serde_json::json!({"output": text}),
                error: Some(stderr),
                truncated: false,
            })
        }
    }
}

pub async fn register_builtin_tools(
    tools: &mut Vec<ToolBox>,
    skills_engine: &SkillsEngine,
    registry: &ToolRegistry,
    skill_runner: &Arc<RwLock<SkillRunner>>,
) {
    tools.push(Arc::new(FileOpTool));
    tools.push(Arc::new(ProcessTool));
    tools.push(Arc::new(ClipboardTool));
    tools.push(Arc::new(ShellTool));
    tools.push(Arc::new(LoadSkillTool {
        skills_engine: skills_engine.clone(),
        registry: registry.clone(),
        skill_runner: skill_runner.clone(),
    }));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::skills::venv::VenvManager;
    use crate::tool::Tool;
    use haven_common::config::SkillsExecConfig;
    use serde_json::json;
    use std::sync::Arc;
    use tempfile::TempDir;
    use tokio::sync::RwLock;
    use tokio_util::sync::CancellationToken;

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
        assert_eq!(FileOpTool.risk_level(&json!({"operation": "copy"})), RiskLevel::Low);
        assert_eq!(FileOpTool.risk_level(&json!({"operation": "read"})), RiskLevel::Safe);
        assert_eq!(FileOpTool.risk_level(&json!({"operation": "list"})), RiskLevel::Safe);
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

    #[test]
    fn test_process_tool_name() {
        assert_eq!(ProcessTool.name(), "process");
    }

    #[test]
    fn test_process_tool_description() {
        assert!(ProcessTool.description().contains("Process operations"));
    }

    #[test]
    fn test_process_tool_risk_level() {
        assert_eq!(ProcessTool.risk_level(&json!({"operation": "kill"})), RiskLevel::Medium);
        assert_eq!(ProcessTool.risk_level(&json!({"operation": "list"})), RiskLevel::Safe);
        assert_eq!(ProcessTool.risk_level(&json!({"operation": "launch"})), RiskLevel::Safe);
    }

    #[test]
    fn test_process_tool_input_schema() {
        let schema = ProcessTool.input_schema();
        assert_eq!(schema["type"].as_str().unwrap(), "object");
        let required = schema["required"].as_array().unwrap();
        let req: Vec<&str> = required.iter().map(|v| v.as_str().unwrap()).collect();
        assert!(req.contains(&"operation"));
        let enum_vals = schema["properties"]["operation"]["enum"].as_array().unwrap();
        let ops: Vec<&str> = enum_vals.iter().map(|v| v.as_str().unwrap()).collect();
        assert!(ops.contains(&"list"));
        assert!(ops.contains(&"launch"));
        assert!(ops.contains(&"kill"));
    }

    #[test]
    fn test_clipboard_tool_name() {
        assert_eq!(ClipboardTool.name(), "clipboard");
    }

    #[test]
    fn test_clipboard_tool_description() {
        assert!(ClipboardTool.description().contains("Clipboard"));
    }

    #[test]
    fn test_clipboard_tool_risk_level() {
        assert_eq!(ClipboardTool.risk_level(&json!({"operation": "write"})), RiskLevel::Low);
        assert_eq!(ClipboardTool.risk_level(&json!({"operation": "read"})), RiskLevel::Safe);
    }

    #[test]
    fn test_clipboard_tool_input_schema() {
        let schema = ClipboardTool.input_schema();
        assert_eq!(schema["type"].as_str().unwrap(), "object");
        let required = schema["required"].as_array().unwrap();
        let req: Vec<&str> = required.iter().map(|v| v.as_str().unwrap()).collect();
        assert!(req.contains(&"operation"));
        let enum_vals = schema["properties"]["operation"]["enum"].as_array().unwrap();
        let ops: Vec<&str> = enum_vals.iter().map(|v| v.as_str().unwrap()).collect();
        assert!(ops.contains(&"read"));
        assert!(ops.contains(&"write"));
    }

    #[tokio::test]
    async fn test_register_builtin_tools() {
        let registry = ToolRegistry::new();
        let skills_engine = SkillsEngine::new();
        let temp_dir = TempDir::new().unwrap();
        let exec_config = SkillsExecConfig {
            venv_root: temp_dir.path().to_path_buf(),
            work_dir: temp_dir.path().to_path_buf(),
            timeout_secs: 30,
            max_output_lines: 5000,
            cpu_time_secs: None,
            max_memory_mb: None,
        };
        let venv_mgr = VenvManager::new(temp_dir.path().to_path_buf());
        let skill_runner = Arc::new(RwLock::new(SkillRunner::new(venv_mgr, exec_config)));

        let mut tools: Vec<ToolBox> = Vec::new();
        register_builtin_tools(&mut tools, &skills_engine, &registry, &skill_runner).await;

        assert_eq!(tools.len(), 5);
        let names: Vec<String> = tools.iter().map(|t| t.name()).collect();
        assert!(names.contains(&"file".to_string()));
        assert!(names.contains(&"process".to_string()));
        assert!(names.contains(&"clipboard".to_string()));
        assert!(names.contains(&"load_skill".to_string()));
        assert!(names.contains(&"shell".to_string()));
    }
}
