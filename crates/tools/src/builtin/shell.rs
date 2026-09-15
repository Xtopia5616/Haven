use async_trait::async_trait;
use haven_common::tools::ToolCatalogGroup;
use haven_common::types::RiskLevel;
use serde_json::Value;
use std::path::PathBuf;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

use crate::live_output::LiveOutputHub;
use crate::{
    ActionService, append_windows_diagnostics, build_shell_command_silent, collect_byte_cap,
    is_progress_clixml, read_stream_capped, sanitize_shell_output, summarize_error,
    write_output_log,
};
use crate::{Tool, ToolExecutionOutcome, ToolResult};

pub struct ShellTool {
    /// Registry of background actions for `background: true` invocations.
    pub actions: Arc<ActionService>,
    /// Live stdout/stderr previews for foreground shell tool cards.
    pub live_outputs: Arc<LiveOutputHub>,
    /// Output cap (chars) for command output.
    pub max_output_chars: usize,
    /// Shell used when the model omits the `shell` argument
    /// ("cmd" | "powershell" | "pwsh" on Windows).
    pub default_shell: String,
}

impl Default for ShellTool {
    fn default() -> Self {
        Self {
            actions: Arc::new(ActionService::new()),
            live_outputs: Arc::new(LiveOutputHub::new()),
            max_output_chars: 20_000,
            #[cfg(windows)]
            default_shell: "powershell".into(),
            #[cfg(not(windows))]
            default_shell: "sh".into(),
        }
    }
}

/// Typed parameters for `ShellTool`. Entry ① (native `run`) and entry ②
/// (`Tool::execute` with LLM JSON) both land in `ShellTool::run`.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct ShellParams {
    /// Shell command to execute.
    pub command: String,
    /// Which shell to run the command in (default: the app-configured shell).
    #[serde(default)]
    pub shell: Option<String>,
    /// If true, hide output from the user (agent always sees it).
    #[serde(default)]
    pub silent: Option<bool>,
    /// Run the command in the background and return an action_id immediately.
    #[serde(default)]
    pub background: Option<bool>,
    /// Working directory to run the command in (default: detected workspace,
    /// otherwise the shared Temp dir).
    #[serde(default)]
    pub cwd: Option<String>,
    /// Private: pre-minted `step-*` id for live `agent:tool_output` events.
    #[serde(default, rename = "_step_id")]
    pub step_id: Option<String>,
    /// Private: owning session id for live output events.
    #[serde(default, rename = "_session_id")]
    pub session_id: Option<String>,
}

impl ShellTool {
    /// Resolve the `shell` and `cwd` arguments, applying the configured
    /// default shell when the caller omits `shell`. Shared by the foreground
    /// and background execution paths.
    fn resolve_shell_and_cwd(
        &self,
        shell_arg: Option<&str>,
        cwd_arg: Option<&str>,
    ) -> (String, Option<PathBuf>) {
        let shell = shell_arg
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| self.default_shell.clone());
        let cwd = cwd_arg
            .filter(|s| !s.is_empty())
            .map(PathBuf::from)
            .or_else(|| {
                std::env::current_dir()
                    .ok()
                    .and_then(|current| haven_common::discover_workspace_root(&current))
            });
        (shell, cwd)
    }

    /// Entry ①: structured native interface (internal code calls — zero
    /// serialization overhead). Entry ② deserializes JSON and delegates here.
    pub async fn run(
        &self,
        params: ShellParams,
        cancel: CancellationToken,
    ) -> anyhow::Result<ToolResult> {
        let cmd = params.command;
        if cmd.is_empty() {
            anyhow::bail!("command is required");
        }
        let silent = params.silent.unwrap_or(false);
        let (shell, cwd) =
            self.resolve_shell_and_cwd(params.shell.as_deref(), params.cwd.as_deref());
        validate_shell_command(&shell, &cmd)?;
        let max_chars = self.max_output_chars;

        if cancel.is_cancelled() {
            anyhow::bail!("cancelled");
        }

        // Background mode: hand the command to the action registry and return
        // immediately. The result is pushed back to the session automatically on
        // completion; the agent can list all actions with the `actions` tool.
        if params.background.unwrap_or(false) {
            let action_id = self
                .actions
                .spawn_shell_for_session(&cmd, &shell, max_chars, cwd, params.session_id.as_deref())
                .await?;
            let mut body = haven_common::tools::background_wait_object(
                "Background action started. If you have no independent foreground work left, END YOUR TURN now with a brief status for the user — do not poll with actions/status. You will be auto-woken with this action's output when it finishes.",
            );
            body.insert("background".into(), serde_json::json!(true));
            body.insert("action_id".into(), serde_json::json!(action_id));
            body.insert("shell".into(), serde_json::json!(shell));
            body.insert("status".into(), serde_json::json!("running"));
            return Ok(ToolResult::ok(serde_json::Value::Object(body)));
        }

        let mut std_cmd = build_shell_command_silent(&shell, &cmd);
        if let Some(cwd) = cwd {
            std_cmd.current_dir(cwd);
        }

        // Suppress console window in silent mode
        if silent {
            #[cfg(windows)]
            {
                use std::os::windows::process::CommandExt;
                std_cmd.creation_flags(crate::CREATE_NO_WINDOW);
            }
        }

        let containment = haven_common::process_containment::ProcessContainment::new()?;
        let mut child = tokio::process::Command::from(std_cmd)
            .kill_on_drop(true)
            .spawn()?;
        let pid = child
            .id()
            .ok_or_else(|| anyhow::anyhow!("shell child did not expose a process id"))?;
        if let Err(error) = containment.attach(pid) {
            let _ = child.kill().await;
            return Err(anyhow::anyhow!(
                "failed to attach shell child to process containment: {}",
                haven_common::error::sanitize_error_text(&error.to_string())
            ));
        }

        if cancel.is_cancelled() {
            // kill_on_drop ensures the child is terminated when `child` drops,
            // but be explicit to avoid a race where the drop hasn't run yet.
            let _ = child.kill().await;
            anyhow::bail!("cancelled");
        }

        // Stream stdout/stderr into capped buffers so huge command output never
        // gets fully buffered (OOM protection). Mirrors network tool behavior.
        // Live tail: while the command runs, push a bounded preview to the
        // tool card via `agent:tool_output` (skipped when silent or no step id).
        let max_collect = collect_byte_cap(max_chars);
        let step_id = params.step_id.clone().unwrap_or_default();
        let session_id = params.session_id.clone().unwrap_or_default();
        let live = !silent && !step_id.is_empty();
        let (tail, running, tail_max) = if live {
            let tail = Arc::new(std::sync::Mutex::new(String::new()));
            let running = Arc::new(std::sync::atomic::AtomicBool::new(true));
            let emit_interval = self.live_outputs.emit_interval().await;
            let tail_max = self.live_outputs.tail_max_chars().await;
            self.live_outputs.spawn_tail_emitter(
                session_id.clone(),
                step_id.clone(),
                Arc::clone(&tail),
                Arc::clone(&running),
                emit_interval,
            );
            (Some(tail), Some(running), tail_max)
        } else {
            (None, None, 0)
        };
        let stdout_fut = read_stream_capped(
            child.stdout.take(),
            max_collect,
            tail.as_ref().map(Arc::clone),
            tail_max,
        );
        let stderr_fut = read_stream_capped(
            child.stderr.take(),
            max_collect,
            tail.as_ref().map(Arc::clone),
            tail_max,
        );
        // Read both pipes concurrently: reading stdout to EOF first can
        // deadlock when the child fills the stderr pipe buffer meanwhile.
        // Keep the whole wait/read operation inside the cancellation race so
        // a foreground shell call does not ignore user cancellation.
        let command_result = async {
            let ((stdout, stdout_overflow), (stderr, stderr_overflow)) =
                tokio::join!(stdout_fut, stderr_fut);
            if let Some(flag) = &running {
                flag.store(false, std::sync::atomic::Ordering::Relaxed);
            }
            let status = child.wait().await?;
            anyhow::Ok(((stdout, stdout_overflow), (stderr, stderr_overflow), status))
        };
        let ((stdout, stdout_overflow), (stderr, stderr_overflow), status) = tokio::select! {
            result = command_result => result?,
            _ = cancel.cancelled() => {
                let _ = child.kill().await;
                if let Some(flag) = &running {
                    flag.store(false, std::sync::atomic::Ordering::Relaxed);
                }
                return Ok(ToolResult::cancelled("shell command cancelled"));
            }
        };

        let mut raw_combined = String::new();
        if !stdout.is_empty() {
            raw_combined.push_str(&stdout);
        }
        if !stderr.is_empty() {
            if !raw_combined.is_empty() {
                raw_combined.push('\n');
            }
            raw_combined.push_str(&stderr);
        }
        // Strip PowerShell's NativeCommandError / CLIXML formatting noise so
        // the reported text carries the real output, not the wrapper.
        let combined = sanitize_shell_output(&raw_combined, &shell);

        let (text, _) = haven_common::encoding::truncate_output(&combined, max_chars);
        let truncated = stdout_overflow || stderr_overflow;
        let exit_code = status.code();
        let mut output = serde_json::json!({"output": text, "shell": shell});
        if let Some(code) = exit_code {
            output["exit_code"] = serde_json::Value::from(code);
        }
        if truncated {
            output["truncated"] = serde_json::Value::Bool(true);
        }
        if status.success() {
            Ok(ToolResult::from_output(output, truncated))
        } else {
            // Non-zero exit: report the failure. Prefer sanitized stderr; if
            // Progress-only CLIXML sanitized to empty, fall back to combined
            // text, then raw stderr so a wiped truncated Error CLIXML is not
            // a silent failure. The full output log is always the pre-sanitize
            // capture so the root cause stays recoverable. Error text is
            // condensed (progress bars dropped, tail kept) and a Windows-trap
            // hint is appended when it matches a common pitfall.
            let sanitized_stderr = sanitize_shell_output(&stderr, &shell);
            let err_source = if !sanitized_stderr.trim().is_empty() {
                sanitized_stderr.as_str()
            } else if !text.trim().is_empty() {
                text.as_str()
            } else if is_progress_clixml(&stderr) {
                // Progress-only CLIXML already sanitized to empty — do not
                // reintroduce the noise via the raw stderr fallback.
                ""
            } else {
                stderr.as_str()
            };
            let mut err_text = summarize_error(err_source, 2000);
            let code_str = exit_code
                .map(|c| c.to_string())
                .unwrap_or_else(|| "unknown".into());
            let log_path = write_output_log(
                "shell-logs",
                &format!(
                    "shell-{}",
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_secs())
                        .unwrap_or(0)
                ),
                &raw_combined,
            );
            let log_path = log_path.to_string_lossy().into_owned();
            err_text = append_windows_diagnostics(&shell, &cmd, &err_text);
            err_text = format!("{}\n[full output: {}]", err_text.trim_end(), log_path);
            let mut result = crate::ToolOutput::new(output, truncated)
                .with_log_path(log_path)
                .into_result();
            result.success = false;
            result.error = Some(format!("exit code {}:\n{}", code_str, err_text));
            result.error_class = Some(crate::ToolErrorClass::Other);
            result.retryability = crate::ToolRetryability::NotRetryable;
            result.outcome = ToolExecutionOutcome::Failed;
            Ok(result)
        }
    }
}

#[async_trait]
impl Tool for ShellTool {
    fn name(&self) -> String {
        "shell".into()
    }
    fn description(&self) -> String {
        crate::prompts::SHELL_DESCRIPTION.into()
    }

    fn catalog_group(&self) -> ToolCatalogGroup {
        ToolCatalogGroup::System
    }

    fn risk_level(&self, _input: &Value) -> RiskLevel {
        RiskLevel::High
    }

    fn timeout_outcome(&self) -> ToolExecutionOutcome {
        ToolExecutionOutcome::TimedOutUnknown
    }

    /// Shell commands get a generous timeout (5 min). A timeout is unknown
    /// because Windows child processes can outlive the shell process.
    fn default_timeout_secs(&self) -> u64 {
        300
    }

    fn input_schema(&self) -> Value {
        #[cfg(windows)]
        let shells = ["cmd", "powershell", "pwsh"];
        #[cfg(not(windows))]
        let shells = ["sh", "bash"];
        let default_shell = self.default_shell.as_str();
        serde_json::json!({
            "type": "object",
            "additionalProperties": false,
            "description": format!("Shell syntax is selected per call. The configured default is `{default_shell}`. Preflight checks reject obvious mismatches: Windows PowerShell 5.1 rejects unquoted && and ||; cmd accepts && and || but not bash substitutions such as $() or POSIX assignments; bash/sh reject cmd expansions such as %VAR% and set VAR=value. Long commands may use background=true; do not poll actions/status."),
            "properties": {
                "command": { "type": "string", "minLength": 1, "description": "Shell command to execute" },
                "shell": { "type": "string", "enum": shells, "description": format!("Which shell to run the command in (default: the shell configured in app settings — `{default_shell}`; pwsh requires PowerShell 7 installed). Match command syntax to this shell; PowerShell 5.1 uses `;` instead of `&&`/`||`." ) },
                "silent": { "type": "boolean", "description": "Only use when the user explicitly requests a quiet tool card; never use it to conceal a side effect (the agent still receives the output)", "default": false },
                "background": { "type": "boolean", "description": "Run the command in the background and return a action_id immediately. Prefer true for long-running work when later steps depend on the result. After launch, if nothing else useful can run in parallel, end your turn — the result is auto-pushed when the action finishes (do not poll).", "default": false },
                "cwd": { "type": "string", "minLength": 1, "description": "Working directory to run the command in. Defaults to the detected workspace root when this process is inside a repository; otherwise the shared Temp sandbox." }
            },
            "required": ["command"]
        })
    }

    fn supports_live_output(&self) -> bool {
        true
    }

    /// Entry ②: LLM JSON entry — convert/validate into `ShellParams`, then
    /// land in the same implementation as entry ①.
    async fn execute(&self, input: Value, cancel: CancellationToken) -> anyhow::Result<ToolResult> {
        let params = crate::tool_contract::parse_tool_input::<ShellParams>(&self.name(), input)?;
        self.run(params, cancel).await
    }

    /// Declare the background-action binding for `background: true` invocations
    /// so the executor attaches the
    /// action to this session without name-matching "shell".
    fn registrations(&self, output: &Value) -> Vec<crate::tool_contract::ToolRegistration> {
        if output.get("background").and_then(|v| v.as_bool()) != Some(true) {
            return Vec::new();
        }
        output
            .get("action_id")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(|action_id| {
                vec![crate::tool_contract::ToolRegistration::Action(
                    action_id.to_string(),
                )]
            })
            .unwrap_or_default()
    }
}

fn validate_shell_command(shell: &str, command: &str) -> anyhow::Result<()> {
    match shell.to_ascii_lowercase().as_str() {
        "powershell" => {
            if contains_unquoted_operator(command, "&&")
                || contains_unquoted_operator(command, "||")
            {
                anyhow::bail!(
                    "shell syntax error before execution: Windows PowerShell 5.1 does not support && or ||; use ';' or select cmd/pwsh"
                );
            }
        }
        "cmd" => {
            if contains_unquoted_operator(command, "$(")
                || contains_unquoted_operator(command, "<(")
                || starts_with_shell_construct(command, "export ")
                || starts_with_shell_construct(command, "source ")
                || starts_with_shell_construct(command, "set -")
                || is_posix_assignment(command)
            {
                anyhow::bail!(
                    "shell syntax error before execution: cmd does not support this bash/sh construct; use cmd syntax or select a POSIX shell"
                );
            }
        }
        "bash" | "sh"
            if contains_unquoted_cmd_variable(command)
                || starts_with_shell_construct(command, "@echo off")
                || starts_with_shell_construct(command, "for /f")
                || starts_with_shell_construct(command, "for /l")
                || starts_with_shell_construct(command, "call :")
                || starts_with_shell_construct(command, "goto ")
                || starts_with_shell_construct(command, "set /")
                || is_cmd_assignment(command) =>
        {
            anyhow::bail!(
                "shell syntax error before execution: bash/sh does not support this cmd construct; use POSIX assignment/expansion syntax or select cmd"
            );
        }
        _ => {}
    }
    Ok(())
}

fn starts_with_shell_construct(command: &str, construct: &str) -> bool {
    command
        .trim_start()
        .to_ascii_lowercase()
        .starts_with(construct)
}

fn is_posix_assignment(command: &str) -> bool {
    let first = command.split_whitespace().next().unwrap_or_default();
    let Some((name, _value)) = first.split_once('=') else {
        return false;
    };
    !name.is_empty()
        && name.chars().enumerate().all(|(index, ch)| {
            (index == 0 && (ch == '_' || ch.is_ascii_alphabetic()))
                || (index > 0 && (ch == '_' || ch.is_ascii_alphanumeric()))
        })
}

fn is_cmd_assignment(command: &str) -> bool {
    let trimmed = command.trim_start();
    if !trimmed.to_ascii_lowercase().starts_with("set ") {
        return false;
    }
    let value = trimmed[4..].trim_start();
    contains_unquoted_operator(value, "=") || (value.starts_with('"') && value[1..].contains('='))
}

fn contains_unquoted_cmd_variable(command: &str) -> bool {
    let chars: Vec<char> = command.chars().collect();
    let mut quote = None;
    for (index, ch) in chars.iter().enumerate() {
        if let Some(active) = quote {
            if *ch == active && (index == 0 || chars[index - 1] != '\\') {
                quote = None;
            }
            continue;
        }
        if *ch == '\'' || *ch == '"' {
            quote = Some(*ch);
            continue;
        }
        if *ch != '%' {
            continue;
        }
        let Some(end) = chars[index + 1..]
            .iter()
            .position(|candidate| *candidate == '%')
        else {
            continue;
        };
        let name = &chars[index + 1..index + 1 + end];
        if !name.is_empty()
            && name
                .iter()
                .all(|candidate| candidate.is_ascii_alphanumeric() || *candidate == '_')
        {
            return true;
        }
    }
    false
}

fn contains_unquoted_operator(command: &str, operator: &str) -> bool {
    let mut quote = None;
    let chars: Vec<char> = command.chars().collect();
    for index in 0..chars.len() {
        let ch = chars[index];
        if let Some(active) = quote {
            if ch == active && (index == 0 || chars[index - 1] != '\\') {
                quote = None;
            }
            continue;
        }
        if ch == '\'' || ch == '"' {
            quote = Some(ch);
            continue;
        }
        let end = index + operator.chars().count();
        if end <= chars.len() && chars[index..end].iter().collect::<String>() == operator {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Tool;
    #[cfg(windows)]
    use base64::Engine;
    use serde_json::json;
    use tempfile::tempdir;

    #[cfg(windows)]
    fn encoded_powershell_command(script: &str) -> String {
        let utf16le: Vec<u8> = script.encode_utf16().flat_map(u16::to_le_bytes).collect();
        base64::engine::general_purpose::STANDARD.encode(utf16le)
    }

    #[test]
    fn test_shell_tool_name() {
        assert_eq!(ShellTool::default().name(), "shell");
    }

    #[test]
    fn test_shell_tool_description() {
        assert!(ShellTool::default().description().contains("shell command"));
    }

    #[test]
    fn test_shell_tool_risk_level() {
        assert_eq!(ShellTool::default().risk_level(&json!({})), RiskLevel::High);
    }

    #[test]
    fn test_shell_tool_input_schema() {
        let schema = ShellTool::default().input_schema();
        assert_eq!(schema["type"].as_str().unwrap(), "object");
        assert!(
            schema["description"]
                .as_str()
                .unwrap()
                .to_ascii_lowercase()
                .contains("preflight")
        );
        let required = schema["required"].as_array().unwrap();
        let req: Vec<&str> = required.iter().map(|v| v.as_str().unwrap()).collect();
        assert!(req.contains(&"command"));
        let shell_enum = schema["properties"]["shell"]["enum"].as_array().unwrap();
        let shells: Vec<&str> = shell_enum.iter().map(|v| v.as_str().unwrap()).collect();
        #[cfg(windows)]
        {
            assert!(shells.contains(&"cmd"));
            assert!(shells.contains(&"powershell"));
            assert!(shells.contains(&"pwsh"));
        }
        #[cfg(not(windows))]
        {
            assert!(shells.contains(&"sh"));
            assert!(shells.contains(&"bash"));
        }
    }

    #[test]
    fn powershell_validation_rejects_unquoted_chain_operators() {
        let error = validate_shell_command("powershell", "Write-Output one && Write-Output two")
            .expect_err("Windows PowerShell 5.1 cannot parse &&");
        assert!(error.to_string().contains("does not support &&"));
        assert!(validate_shell_command("powershell", "Write-Output 'a && b'").is_ok());
        assert!(validate_shell_command("pwsh", "Write-Output one && Write-Output two").is_ok());
    }

    #[test]
    fn cmd_validation_rejects_obvious_posix_constructs() {
        for command in [
            "echo $(Get-Date)",
            "export NAME=value",
            "source ./env.sh",
            "NAME=value echo ready",
        ] {
            let error = validate_shell_command("cmd", command)
                .expect_err("cmd should reject an obvious POSIX construct");
            assert!(error.to_string().contains("before execution"));
        }
        assert!(validate_shell_command("cmd", "echo '$(not-a-command)'").is_ok());
        assert!(validate_shell_command("cmd", "echo ok && echo ready").is_ok());
    }

    #[test]
    fn posix_validation_rejects_obvious_cmd_constructs() {
        for command in [
            "echo %PATH%",
            "set NAME=value",
            "set \"NAME=value\"",
            "for /f %i in (file) do @echo %i",
            "@echo off",
        ] {
            let error = validate_shell_command("bash", command)
                .expect_err("bash should reject an obvious cmd construct");
            assert!(error.to_string().contains("before execution"));
        }
        assert!(validate_shell_command("bash", "echo '$PATH'").is_ok());
        assert!(validate_shell_command("sh", "NAME=value echo ready").is_ok());
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn test_shell_large_output_reports_success_and_truncates() {
        // ~160KB of output exceeds the collection cap (80KB default). The child
        // must complete normally (no broken pipe from closing the read end
        // early) and the result must carry the truncation flag.
        let result = ShellTool::default()
            .execute(
                json!({
                    "command": "for /l %i in (1,1,5000) do @echo aaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                    "shell": "cmd",
                }),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(
            result.success,
            "child must complete successfully: {:?}",
            result.error
        );
        let out = result.output["output"].as_str().unwrap();
        assert!(
            out.len() < 50_000,
            "output should be capped, got {} bytes",
            out.len()
        );
        assert!(
            result.output["truncated"].as_bool().unwrap_or(false),
            "capped output must carry the truncated flag"
        );
        assert!(
            result.truncated,
            "transport result must carry the same flag"
        );
    }

    #[tokio::test]
    async fn test_shell_missing_command() {
        let result = ShellTool::default()
            .execute(json!({"command": ""}), CancellationToken::new())
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_shell_execute_cancelled() {
        let cancel = CancellationToken::new();
        cancel.cancel();
        let result = ShellTool::default()
            .execute(json!({"command": "echo hi"}), cancel)
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_shell_timeout_is_unknown_without_replay() {
        #[cfg(windows)]
        let command = "Start-Sleep -Seconds 5";
        #[cfg(not(windows))]
        let command = "sleep 5";
        let result = ShellTool::default()
            .execute_with_timeout(json!({"command": command}), CancellationToken::new(), 1)
            .await
            .unwrap();
        assert_eq!(result.outcome, ToolExecutionOutcome::TimedOutUnknown);
        assert!(!result.success);
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn test_shell_cmd_echo() {
        let result = ShellTool::default()
            .execute(
                json!({"command": "echo hello", "shell": "cmd"}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(result.success, "expected success: {:?}", result.error);
        assert!(result.output["output"].as_str().unwrap().contains("hello"));
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn test_shell_powershell_echo() {
        let result = ShellTool::default()
            .execute(
                json!({"command": "Write-Output hello", "shell": "powershell"}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(result.success, "expected success: {:?}", result.error);
        assert!(result.output["output"].as_str().unwrap().contains("hello"));
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn test_shell_pwsh_echo() {
        // PowerShell 7 (pwsh) is not preinstalled on Windows; skip when absent.
        if std::process::Command::new("pwsh")
            .args(["-NoProfile", "-Command", "$PSVersionTable.PSVersion.Major"])
            .output()
            .map(|o| !o.status.success())
            .unwrap_or(true)
        {
            eprintln!("pwsh not installed; skipping pwsh echo test");
            return;
        }
        let result = ShellTool::default()
            .execute(
                json!({"command": "Write-Output hello-pwsh", "shell": "pwsh"}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(result.success, "expected success: {:?}", result.error);
        assert!(
            result.output["output"]
                .as_str()
                .unwrap()
                .contains("hello-pwsh"),
            "got: {:?}",
            result.output["output"]
        );
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn test_shell_respects_configured_default_shell() {
        // When the model omits `shell`, the configured default is used.
        let tool = ShellTool {
            default_shell: "cmd".into(),
            ..Default::default()
        };
        let result = tool
            .execute(
                json!({"command": "echo default-shell-ok"}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(result.success, "expected success: {:?}", result.error);
        assert_eq!(result.output["shell"], "cmd");
        assert!(
            result.output["output"]
                .as_str()
                .unwrap()
                .contains("default-shell-ok")
        );
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn test_shell_nonzero_exit_reports_failure() {
        let result = ShellTool::default()
            .execute(
                json!({"command": "exit 42", "shell": "cmd"}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(!result.success, "non-zero exit must report failure");
        assert!(result.error.is_some());
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn test_shell_nonzero_exit_without_stderr_has_nonempty_error() {
        // A failing command with no stderr must not produce an empty error:
        // the result text flows straight into the model's observation, and an
        // empty string looks like the tool never returned anything.
        let result = ShellTool::default()
            .execute(
                json!({"command": "echo out && exit 42", "shell": "cmd"}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(!result.success);
        let err = result.error.unwrap();
        assert!(
            !err.trim().is_empty(),
            "error must not be empty, got: {:?}",
            err
        );
        assert!(
            err.contains("out"),
            "error should carry the stdout content when stderr is empty, got: {:?}",
            err
        );
        assert!(
            err.contains("exit code 42"),
            "error should state the exit code, got: {:?}",
            err
        );
        assert_eq!(result.output["exit_code"], 42);
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn test_shell_reports_shell_and_exit_code_on_success() {
        let result = ShellTool::default()
            .execute(
                json!({"command": "echo hello", "shell": "cmd"}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(result.success);
        assert_eq!(result.output["shell"], "cmd");
        assert_eq!(result.output["exit_code"], 0);
        assert!(result.output["output"].as_str().unwrap().contains("hello"));
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn test_shell_powershell_echo_chinese() {
        // Non-ASCII input/output through the real -EncodedCommand pipeline:
        // the command text and PowerShell cmdlet output must survive as UTF-8.
        let result = ShellTool::default()
            .execute(
                json!({"command": "Write-Output 你好", "shell": "powershell"}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(result.success, "expected success: {:?}", result.error);
        let out = result.output["output"].as_str().unwrap();
        assert!(
            out.contains("你好"),
            "chinese cmdlet output must survive: {out:?}"
        );
        assert!(!out.contains('\u{FFFD}'), "no replacement chars: {out:?}");
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn test_shell_powershell_echo_chinese_in_quotes() {
        // The command contains quotes, semicolons and % alongside Chinese —
        // exactly what -Command re-parsing used to break.
        let result = ShellTool::default()
            .execute(
                json!({"command": "Write-Output \"值 100%; 你好\"", "shell": "powershell"}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(result.success, "expected success: {:?}", result.error);
        let out = result.output["output"].as_str().unwrap();
        assert!(out.contains("你好"), "got: {out:?}");
        assert!(out.contains("100%"), "got: {out:?}");
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn test_shell_cmd_echo_chinese() {
        // cmd passes the command as a UTF-16 argument; chcp 65001 makes cmd
        // output UTF-8. Chinese text must survive both directions.
        let result = ShellTool::default()
            .execute(
                json!({"command": "echo 你好", "shell": "cmd"}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(result.success, "expected success: {:?}", result.error);
        assert!(
            result.output["output"].as_str().unwrap().contains("你好"),
            "got: {:?}",
            result.output["output"]
        );
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn test_shell_cmd_captures_gbk_native_output() {
        // Emit raw GBK bytes (CP936: 你好 = C4 E3 BA C3) without depending
        // on a PATH-provided interpreter. The cmd capture must use the GBK
        // fallback instead of mangling the byte stream.
        let emit_gbk = format!(
            "powershell -NoProfile -EncodedCommand {}",
            encoded_powershell_command(
                "[Console]::OpenStandardOutput().Write([byte[]](0xC4,0xE3,0xBA,0xC3,0x0A),0,5)"
            )
        );
        let result = ShellTool::default()
            .execute(
                json!({"command": emit_gbk, "shell": "cmd"}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(result.success, "expected success: {:?}", result.error);
        let out = result.output["output"].as_str().unwrap();
        assert!(
            out.contains("你好"),
            "GBK native output must be decoded: {out:?}"
        );
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn test_shell_powershell_captures_gbk_native_output() {
        // Direct raw output bypasses PowerShell's text encoder so this checks
        // that the capture path preserves GBK bytes for decode_lossy.
        let emit_gbk =
            "[Console]::OpenStandardOutput().Write([byte[]](0xC4,0xE3,0xBA,0xC3,0x0A),0,5)";
        let result = ShellTool::default()
            .execute(
                json!({"command": emit_gbk, "shell": "powershell"}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(result.success, "expected success: {:?}", result.error);
        let out = result.output["output"].as_str().unwrap();
        assert!(
            out.contains("你好"),
            "GBK native output must be decoded: {out:?}"
        );
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn test_shell_powershell_strips_native_command_error_noise() {
        // A successful PowerShell command whose stderr carries PS error-record
        // formatting must report the real message without the wrapper noise.
        let result = ShellTool::default()
            .execute(
                json!({"command": "Write-Output ok; cmd /c \"echo noise 1>&2\"", "shell": "powershell"}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(
            result.success,
            "exit 0 with stderr is still a success: {:?}",
            result.error
        );
        let out = result.output["output"].as_str().unwrap_or("");
        assert!(
            !out.contains("NativeCommandError"),
            "wrapper noise must be stripped, got: {}",
            out
        );
        assert!(out.contains("ok"), "got: {}", out);
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn test_shell_powershell_clixml_stderr_stripped() {
        // Merging native stderr (`2>&1`) makes PS 5.1 serialize the error
        // stream as a CLIXML document; the real message must survive without
        // the wrapper, the `_xHHHH_` char escapes or the localized noise.
        // (The merged error record makes PS exit 1 — that is expected here.)
        let result = ShellTool::default()
            .execute(
                json!({"command": "cmd /c \"echo boom 1>&2\" 2>&1", "shell": "powershell"}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        let out = result.output["output"].as_str().unwrap();
        assert!(out.contains("boom"), "real message must survive: {out:?}");
        assert!(!out.contains("CLIXML"), "wrapper must be stripped: {out:?}");
        assert!(
            !out.contains("_x000D_"),
            "char escapes must be decoded: {out:?}"
        );
        assert!(
            !out.contains("CategoryInfo"),
            "record noise must be dropped: {out:?}"
        );
        assert_eq!(result.output["exit_code"], 1);
        let err = result.error.as_deref().unwrap_or("");
        assert!(
            err.contains("cmd : boom"),
            "error must carry the message: {err:?}"
        );
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn test_shell_utf16_redirection_decodes() {
        // A UTF-16LE file (what PS 5.1 `>` redirection used to write) read
        // back through the shell pipe must decode cleanly instead of arriving
        // as NUL-byte mojibake.
        let temp_dir = tempdir().unwrap();
        let path = temp_dir.path().join("utf16_redir_test.txt");
        let script = format!(
            "[IO.File]::WriteAllText('{}', '你好', [Text.Encoding]::Unicode)",
            path.display()
        );
        let write = ShellTool::default()
            .execute(
                json!({"command": script, "shell": "powershell"}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(write.success, "write failed: {:?}", write.error);
        let read = ShellTool::default()
            .execute(
                json!({"command": format!("type {}", path.display()), "shell": "cmd"}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(read.success, "read failed: {:?}", read.error);
        let out = read.output["output"].as_str().unwrap();
        assert!(out.contains("你好"), "UTF-16 file must decode: {out:?}");
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn test_shell_powershell_redirection_roundtrip_chinese() {
        // `>` redirection on PS 5.1 must not produce a UTF-16 file: writing
        // with `>` then reading the bytes back through the shell pipe must
        // round-trip Chinese cleanly (no NUL bytes, no replacement chars).
        let temp_dir = tempdir().unwrap();
        let path = temp_dir.path().join("redir_utf8_test.txt");
        let write = ShellTool::default()
            .execute(
                json!({"command": format!("\"中文内容 hello\" > '{}'", path.display()), "shell": "powershell"}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(write.success, "write failed: {:?}", write.error);
        let read = ShellTool::default()
            .execute(
                json!({"command": format!("type {}", path.display()), "shell": "cmd"}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(read.success, "read failed: {:?}", read.error);
        let out = read.output["output"].as_str().unwrap();
        assert!(
            out.contains("中文内容"),
            "redirection round-trip must be UTF-8: {out:?}"
        );
        assert!(!out.contains('\u{FFFD}'), "no replacement chars: {out:?}");
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn test_shell_captures_stderr() {
        let result = ShellTool::default()
            .execute(
                json!({"command": "echo boom 1>&2", "shell": "cmd"}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(
            result.success,
            "echo to stderr still exits 0: {:?}",
            result.error
        );
        assert!(result.output["output"].as_str().unwrap().contains("boom"));
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn test_shell_background_returns_action_id_and_completes() {
        let tool = ShellTool::default();
        let result = tool
            .execute(
                json!({"command": "echo bg-result", "shell": "cmd", "background": true}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(result.success);
        assert_eq!(result.output["background"], true);
        assert_eq!(result.output["status"], "running");
        let action_id = result.output["action_id"].as_str().unwrap().to_string();
        assert!(!action_id.is_empty());

        // Poll the action registry until the action completes.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let status = loop {
            let v = tool.actions.status(&action_id).await;
            if v["status"] != "running" || std::time::Instant::now() > deadline {
                break v;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        };
        assert_eq!(status["status"], "completed", "got: {}", status);
        assert!(status["output"].as_str().unwrap().contains("bg-result"));
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn test_shell_background_empty_command_rejected() {
        let result = ShellTool::default()
            .execute(
                json!({"command": "", "background": true}),
                CancellationToken::new(),
            )
            .await;
        assert!(result.is_err());
    }

    #[test]
    fn test_shell_background_schema_field() {
        let schema = ShellTool::default().input_schema();
        assert_eq!(
            schema["properties"]["background"]["type"], "boolean",
            "background field must be in the schema"
        );
    }
}
