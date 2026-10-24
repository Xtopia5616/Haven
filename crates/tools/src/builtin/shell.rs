use async_trait::async_trait;
use haven_common::tools::ToolCatalogGroup;
use haven_common::types::RiskLevel;
use serde_json::Value;
use std::path::PathBuf;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

use crate::execution_workspace::ExecutionWorkspace;
use crate::live_output::LiveOutputHub;
#[cfg(not(test))]
use crate::write_output_log;
#[cfg(test)]
use crate::write_shell_output_log_for_test;
use crate::{Tool, ToolExecutionOutcome, ToolResult};
use crate::{
    append_windows_diagnostics, build_shell_command_silent, collect_byte_cap, is_progress_clixml,
    read_stream_text_capped, sanitize_shell_output, summarize_error,
};

pub(crate) struct ShellTool {
    /// Live stdout/stderr previews for foreground shell tool cards.
    pub live_outputs: Arc<LiveOutputHub>,
    /// Output cap (chars) for command output.
    pub max_output_chars: usize,
    /// Shell used when the model omits the `shell` argument
    /// ("cmd" | "powershell" | "pwsh" on Windows).
    pub default_shell: String,
    /// Shared backend root used when the model omits `cwd`.
    pub(crate) execution_workspace: ExecutionWorkspace,
    /// Isolated output-log root for shell tests.
    #[cfg(test)]
    pub(crate) test_output_log_root: Option<PathBuf>,
}

impl Default for ShellTool {
    fn default() -> Self {
        Self {
            live_outputs: Arc::new(LiveOutputHub::new()),
            max_output_chars: 20_000,
            #[cfg(windows)]
            default_shell: "powershell".into(),
            #[cfg(not(windows))]
            default_shell: "sh".into(),
            execution_workspace: ExecutionWorkspace::discover(),
            #[cfg(test)]
            test_output_log_root: None,
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

/// Shell executable and working directory selected for one command.
struct ResolvedShellContext {
    shell: String,
    working_directory: Option<PathBuf>,
}

impl ShellTool {
    /// Resolve the `shell` and `cwd` arguments, applying the configured
    /// default shell when the caller omits `shell`. Shared by the foreground
    /// and background execution paths.
    fn resolve_shell_context(
        &self,
        shell_arg: Option<&str>,
        cwd_arg: Option<&str>,
        execution_workspace: Option<&crate::ExecutionWorkspaceSnapshot>,
    ) -> ResolvedShellContext {
        let shell = shell_arg
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| self.default_shell.clone());
        let cwd = cwd_arg
            .filter(|s| !s.is_empty())
            .map(PathBuf::from)
            .or_else(|| {
                execution_workspace
                    .and_then(|workspace| workspace.root.clone())
                    .or_else(|| self.execution_workspace.root().map(PathBuf::from))
            });
        ResolvedShellContext {
            shell,
            working_directory: cwd,
        }
    }

    /// Run a shell command through the normal host-process execution path.
    pub async fn run(
        &self,
        params: ShellParams,
        cancel: CancellationToken,
    ) -> anyhow::Result<ToolResult> {
        self.run_with_execution_context(params, cancel, None).await
    }

    async fn run_with_execution_context(
        &self,
        params: ShellParams,
        cancel: CancellationToken,
        execution_context: Option<&crate::ToolExecutionContext>,
    ) -> anyhow::Result<ToolResult> {
        let cmd = params.command;
        if cmd.is_empty() {
            anyhow::bail!("command is required");
        }
        let silent = params.silent.unwrap_or(false);
        let workspace_snapshot = execution_context.and_then(|context| {
            context
                .execution_workspace
                .as_ref()
                .map(crate::execution_workspace::ExecutionWorkspace::snapshot)
        });
        let shell_context = self.resolve_shell_context(
            params.shell.as_deref(),
            params.cwd.as_deref(),
            workspace_snapshot.as_ref(),
        );
        validate_shell_command(&shell_context.shell, &cmd)?;
        let max_chars = self.max_output_chars;

        let sandbox_mode = match execution_context {
            Some(context) => {
                let permit = context
                    .acquire_execution_policy_permit()
                    .await
                    .map_err(crate::security::execution_policy_denied)?
                    .ok_or_else(|| {
                        crate::security::execution_policy_denied(
                            "Shell execution has no live policy authorization".into(),
                        )
                    })?;
                let mode = permit.sandbox_mode();
                drop(permit);
                mode
            }
            None => haven_common::types::SandboxMode::FullAccess,
        };

        if sandbox_mode == haven_common::types::SandboxMode::ReadOnly {
            return Err(crate::security::execution_policy_denied(
                "Shell execution is blocked by read-only sandbox mode".into(),
            ));
        }
        if cancel.is_cancelled() {
            anyhow::bail!("cancelled");
        }

        let mut std_cmd = build_shell_command_silent(&shell_context.shell, &cmd);
        if let Some(working_directory) = shell_context.working_directory {
            std_cmd.current_dir(working_directory);
        }

        let execution_authorization =
            execution_context.and_then(crate::ToolExecutionContext::clone_execution_authorization);
        let spawn_permit = match execution_context {
            Some(context) => context
                .acquire_execution_policy_permit()
                .await
                .map_err(crate::security::execution_policy_denied)?,
            None => None,
        };
        let spawn_mode = spawn_permit
            .as_ref()
            .map_or(haven_common::types::SandboxMode::FullAccess, |permit| {
                permit.sandbox_mode()
            });
        if spawn_mode == haven_common::types::SandboxMode::ReadOnly {
            return Err(crate::security::execution_policy_denied(
                "Shell execution is blocked by read-only sandbox mode".into(),
            ));
        }
        let mut child = match spawn_mode {
            haven_common::types::SandboxMode::ReadOnly => {
                return Err(crate::security::execution_policy_denied(
                    "Shell execution is blocked by read-only sandbox mode".into(),
                ));
            }
            haven_common::types::SandboxMode::FullAccess => {
                spawn_shell_with_host_permissions(std_cmd, silent).await?
            }
            haven_common::types::SandboxMode::WorkspaceWrite => {
                #[cfg(windows)]
                {
                    let permit = spawn_permit.as_ref().ok_or_else(|| {
                        crate::security::execution_policy_denied(
                            "Shell execution has no live policy authorization".into(),
                        )
                    })?;
                    let write_root_capabilities = permit
                        .workspace_write_directories()
                        .map_err(crate::security::execution_policy_denied)?;
                    let roots = write_root_capabilities
                        .iter()
                        .map(|directory| directory.root().to_path_buf())
                        .collect();
                    let request = sandboxed_shell_request(
                        &std_cmd,
                        roots,
                        !matches!(
                            permit.network_policy(),
                            haven_common::types::NetworkPolicy::Deny
                                | haven_common::types::NetworkPolicy::Restricted
                        ),
                        matches!(
                            shell_context.shell.to_ascii_lowercase().as_str(),
                            "powershell" | "pwsh"
                        ),
                    )?;
                    let process =
                        haven_platform::sandboxed_process::SandboxedProcess::spawn(request)
                            .await
                            .map_err(|error| {
                                // The platform boundary already sanitizes and bounds each MXC
                                // diagnostic before composing the ordered retry report. Keep
                                // the full report for the model; sanitize the log copy again.
                                let reason = error.to_string();
                                tracing::warn!(
                                    error = %haven_common::error::sanitize_error_text(&reason),
                                    "MXC rejected the Workspace Write shell process"
                                );
                                crate::security::execution_policy_denied(format!(
                                    "MXC 无法在 WorkspaceWrite 隔离中启动 Shell：{reason}"
                                ))
                            })?;
                    let sandbox_notice = process.degradation_notice().map(str::to_owned);
                    ShellProcess {
                        kind: ShellProcessKind::Sandboxed(process),
                        sandbox_notice,
                    }
                }
                #[cfg(not(windows))]
                {
                    let _ = std_cmd;
                    return Err(crate::security::execution_policy_denied(
                        "MXC Workspace Write process isolation requires Windows 11".into(),
                    ));
                }
            }
        };
        drop(spawn_permit);

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
        let (tail, running) = if live {
            let tail = self.live_outputs.new_tail().await;
            let running = Arc::new(std::sync::atomic::AtomicBool::new(true));
            let emit_interval = self.live_outputs.emit_interval().await;
            self.live_outputs.spawn_tail_emitter(
                session_id.clone(),
                step_id.clone(),
                tail.clone(),
                Arc::clone(&running),
                emit_interval,
            );
            (Some(tail), Some(running))
        } else {
            (None, None)
        };
        let stdout_fut =
            read_stream_text_capped(child.take_stdout(), max_collect, tail.as_ref().cloned());
        let stderr_fut =
            read_stream_text_capped(child.take_stderr(), max_collect, tail.as_ref().cloned());
        // Read both pipes concurrently: reading stdout to EOF first can
        // deadlock when the child fills the stderr pipe buffer meanwhile.
        // Keep the whole wait/read operation inside the cancellation race so
        // a foreground shell call does not ignore user cancellation.
        let command_result = async {
            let (stdout, stderr) = tokio::join!(stdout_fut, stderr_fut);
            if let Some(flag) = &running {
                flag.store(false, std::sync::atomic::Ordering::Relaxed);
            }
            let status = child.wait().await?;
            anyhow::Ok((stdout, stderr, status))
        };
        let policy_revoked = async {
            match execution_authorization {
                Some(authorization) => authorization.wait_for_policy_revocation().await,
                None => std::future::pending::<()>().await,
            }
        };
        tokio::pin!(policy_revoked);
        let (stdout, stderr, status) = tokio::select! {
            result = command_result => result?,
            _ = cancel.cancelled() => {
                let _ = child.kill().await;
                if let Some(flag) = &running {
                    flag.store(false, std::sync::atomic::Ordering::Relaxed);
                }
                return Ok(ToolResult::cancelled("shell command cancelled"));
            }
            _ = &mut policy_revoked => {
                let _ = child.kill().await;
                if let Some(flag) = &running {
                    flag.store(false, std::sync::atomic::Ordering::Relaxed);
                }
                return Ok(ToolResult::cancelled("execution policy changed; shell command cancelled"));
            }
        };

        let truncated = stdout.overflowed || stderr.overflowed;
        let mut raw_combined = String::new();
        if !stdout.text.is_empty() {
            raw_combined.push_str(&stdout.text);
        }
        if !stderr.text.is_empty() {
            if !raw_combined.is_empty() {
                raw_combined.push('\n');
            }
            raw_combined.push_str(&stderr.text);
        }
        // Strip PowerShell's NativeCommandError / CLIXML formatting noise so
        // the reported text carries the real output, not the wrapper.
        let combined = sanitize_shell_output(&raw_combined, &shell_context.shell);

        let text = haven_common::encoding::truncate_output(&combined, max_chars).text;
        let exit_code = status.code;
        let mut output = serde_json::json!({"output": text, "shell": shell_context.shell});
        if let Some(notice) = child.sandbox_notice() {
            output["sandbox_notice"] = serde_json::Value::String(notice.to_owned());
        }
        if let Some(code) = exit_code {
            output["exit_code"] = serde_json::Value::from(code);
        }
        if status.success {
            Ok(ToolResult::from_output(output, truncated))
        } else {
            // Non-zero exit: report the failure. Prefer sanitized stderr; if
            // Progress-only CLIXML sanitized to empty, fall back to combined
            // text, then raw stderr so a wiped truncated Error CLIXML is not
            // a silent failure. The full output log is always the pre-sanitize
            // capture so the root cause stays recoverable. Error text is
            // condensed (progress bars dropped, tail kept) and a Windows-trap
            // hint is appended when it matches a common pitfall.
            let sanitized_stderr = sanitize_shell_output(&stderr.text, &shell_context.shell);
            let err_source = if !sanitized_stderr.trim().is_empty() {
                sanitized_stderr.as_str()
            } else if !text.trim().is_empty() {
                text.as_str()
            } else if is_progress_clixml(&stderr.text) {
                // Progress-only CLIXML already sanitized to empty — do not
                // reintroduce the noise via the raw stderr fallback.
                ""
            } else {
                stderr.text.as_str()
            };
            let mut err_text = summarize_error(err_source, 2000);
            let code_str = exit_code
                .map(|c| c.to_string())
                .unwrap_or_else(|| "unknown".into());
            let file_stem = format!("shell-{}", uuid::Uuid::new_v4().simple());
            #[cfg(test)]
            let log_path = self.test_output_log_root.as_ref().map_or_else(
                || PathBuf::from(format!("test-shell-logs/{file_stem}.log")),
                |root| write_shell_output_log_for_test(root, &file_stem, &raw_combined),
            );
            #[cfg(not(test))]
            let log_path = write_output_log("shell-logs", &file_stem, &raw_combined);
            let log_path = log_path.to_string_lossy().into_owned();
            err_text = append_windows_diagnostics(&shell_context.shell, &cmd, &err_text);
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

    fn authorization_resources(
        &self,
        input: &Value,
    ) -> Result<Vec<crate::security::DeclaredResource>, String> {
        let params = crate::tool_contract::parse_tool_input::<ShellParams>("shell", input.clone())
            .map_err(|error| error.to_string())?;
        Ok(params
            .cwd
            .filter(|cwd| !cwd.trim().is_empty())
            .map(|cwd| {
                vec![crate::security::DeclaredResource::LocalPath {
                    path: PathBuf::from(cwd),
                    access: crate::security::LocalPathAccess::Read,
                }]
            })
            .unwrap_or_default())
    }

    fn catalog_group(&self) -> ToolCatalogGroup {
        ToolCatalogGroup::Shell
    }

    fn risk_level(&self, _input: &Value) -> RiskLevel {
        RiskLevel::High
    }

    fn operation_spec(&self) -> Option<crate::operation_view::OperationSpec> {
        let mut spec = crate::operation_view::root_tool_spec(
            "shell",
            crate::prompts::SHELL_DESCRIPTION,
            self.input_schema(),
            RiskLevel::High,
            crate::OperationIdempotency::Unknown,
            crate::ToolOperationScope::Session,
            crate::ToolConcurrency::Exclusive,
            ToolCatalogGroup::Shell,
            None,
        );
        spec.policy.execution_boundary = crate::ExecutionBoundary::SandboxedHost;
        Some(spec)
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
            "description": format!("Shell syntax is selected per call. The configured default is `{default_shell}`. Preflight checks reject obvious mismatches: Windows PowerShell 5.1 rejects unquoted && and ||; cmd accepts && and || but not bash substitutions such as $() or POSIX assignments; bash/sh reject cmd expansions such as %VAR% and set VAR=value. Use toolrun.start with tool_name=shell for long-running commands."),
            "properties": {
                "command": { "type": "string", "minLength": 1, "description": "Shell command to execute" },
                "shell": { "type": "string", "enum": shells, "description": format!("Which shell to run the command in (default: the shell configured in app settings — `{default_shell}`; pwsh requires PowerShell 7 installed). Match command syntax to this shell; PowerShell 5.1 uses `;` instead of `&&`/`||`." ) },
                "silent": { "type": "boolean", "description": "Only use when the user explicitly requests a quiet tool card; never use it to conceal a side effect (the agent still receives the output)", "default": false },
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

    async fn execute_with_authorization_context(
        &self,
        input: Value,
        cancel: CancellationToken,
        target_binding: Option<&str>,
        context: &crate::ToolExecutionContext,
    ) -> anyhow::Result<ToolResult> {
        if target_binding.is_some() {
            anyhow::bail!("shell tool cannot enforce an external authorization target");
        }
        let params = crate::tool_contract::parse_tool_input::<ShellParams>(&self.name(), input)?;
        self.run_with_execution_context(params, cancel, Some(context))
            .await
    }
}

type ShellAsyncReader = Box<dyn tokio::io::AsyncRead + Send + Unpin>;

enum ShellProcessKind {
    Host {
        child: tokio::process::Child,
        _containment: haven_platform::process_containment::ProcessContainment,
    },
    #[cfg(windows)]
    Sandboxed(haven_platform::sandboxed_process::SandboxedProcess),
}

struct ShellProcess {
    kind: ShellProcessKind,
    sandbox_notice: Option<String>,
}

struct ShellExitStatus {
    code: Option<i32>,
    success: bool,
}

impl ShellProcess {
    fn sandbox_notice(&self) -> Option<&str> {
        self.sandbox_notice.as_deref()
    }

    fn take_stdout(&mut self) -> Option<ShellAsyncReader> {
        match &mut self.kind {
            ShellProcessKind::Host { child, .. } => child
                .stdout
                .take()
                .map(|stream| Box::new(stream) as ShellAsyncReader),
            #[cfg(windows)]
            ShellProcessKind::Sandboxed(child) => child
                .take_stdout()
                .map(|stream| Box::new(stream) as ShellAsyncReader),
        }
    }

    fn take_stderr(&mut self) -> Option<ShellAsyncReader> {
        match &mut self.kind {
            ShellProcessKind::Host { child, .. } => child
                .stderr
                .take()
                .map(|stream| Box::new(stream) as ShellAsyncReader),
            #[cfg(windows)]
            ShellProcessKind::Sandboxed(child) => child
                .take_stderr()
                .map(|stream| Box::new(stream) as ShellAsyncReader),
        }
    }

    async fn wait(&mut self) -> std::io::Result<ShellExitStatus> {
        match &mut self.kind {
            ShellProcessKind::Host { child, .. } => {
                let status = child.wait().await?;
                Ok(ShellExitStatus {
                    code: status.code(),
                    success: status.success(),
                })
            }
            #[cfg(windows)]
            ShellProcessKind::Sandboxed(child) => {
                let code = child.wait().await?;
                Ok(ShellExitStatus {
                    code: Some(code),
                    success: code == 0,
                })
            }
        }
    }

    async fn kill(&mut self) -> std::io::Result<()> {
        match &mut self.kind {
            ShellProcessKind::Host { child, .. } => child.kill().await,
            #[cfg(windows)]
            ShellProcessKind::Sandboxed(child) => child.kill().await,
        }
    }
}

async fn spawn_shell_with_host_permissions(
    std_cmd: std::process::Command,
    silent: bool,
) -> anyhow::Result<ShellProcess> {
    let containment = haven_platform::process_containment::ProcessContainment::new()?;
    let mut child_cmd = tokio::process::Command::from(std_cmd);
    #[cfg(windows)]
    let windows_creation_flags = if silent { crate::CREATE_NO_WINDOW } else { 0 };
    #[cfg(not(windows))]
    let windows_creation_flags = {
        let _ = silent;
        0
    };
    containment.prepare_command(child_cmd.as_std_mut(), windows_creation_flags);
    let child_spawn_result = {
        let _spawn_guard = haven_platform::process_containment::process_spawn_lock();
        child_cmd.kill_on_drop(true).spawn()
    };
    let mut child = child_spawn_result?;
    let pid = child
        .id()
        .ok_or_else(|| anyhow::anyhow!("shell child did not expose a process id"))?;
    #[cfg(windows)]
    let attach_result = child
        .raw_handle()
        .ok_or_else(|| std::io::Error::other("shell child process handle is unavailable"))
        .and_then(|handle| containment.attach_and_resume(pid, handle));
    #[cfg(not(windows))]
    let attach_result = containment.attach_and_resume(pid, ());
    if let Err(error) = attach_result {
        let _ = child.kill().await;
        return Err(anyhow::anyhow!(
            "failed to attach shell child to process containment: {}",
            haven_common::error::sanitize_error_text(&error.to_string())
        ));
    }
    Ok(ShellProcess {
        kind: ShellProcessKind::Host {
            child,
            _containment: containment,
        },
        sandbox_notice: None,
    })
}

#[cfg(windows)]
fn sandboxed_shell_request(
    std_cmd: &std::process::Command,
    readwrite_paths: Vec<PathBuf>,
    allow_network: bool,
    allow_desktop_handle_access: bool,
) -> anyhow::Result<haven_platform::sandboxed_process::SandboxedProcessRequest> {
    use haven_platform::sandboxed_process::SandboxedProcessRequest;
    use std::ffi::{OsStr, OsString};

    let mut environment = std_cmd
        .get_envs()
        .filter_map(|(key, value)| value.map(|value| (key.to_os_string(), value.to_os_string())))
        .collect::<Vec<_>>();
    let temp_root = readwrite_paths
        .first()
        .ok_or_else(|| anyhow::anyhow!("Workspace Write has no MXC writable root"))?;
    environment.retain(|(key, _)| {
        !os_key_eq_ignore_ascii_case(key, "TEMP") && !os_key_eq_ignore_ascii_case(key, "TMP")
    });
    environment.push((OsString::from("TEMP"), temp_root.as_os_str().to_os_string()));
    environment.push((OsString::from("TMP"), temp_root.as_os_str().to_os_string()));

    let mut readonly_paths = Vec::new();
    if let Some(system_root) = environment
        .iter()
        .find(|(key, _)| os_key_eq_ignore_ascii_case(key, "SYSTEMROOT"))
        .map(|(_, value)| PathBuf::from(value))
    {
        readonly_paths.push(system_root.clone());
        readonly_paths.push(system_root.join("System32"));
        readonly_paths.push(system_root.join(r"System32\WindowsPowerShell\v1.0"));
    }
    for path in environment
        .iter()
        .find(|(key, _)| os_key_eq_ignore_ascii_case(key, "PATH"))
        .map(|(_, value)| std::env::split_paths(value).collect::<Vec<_>>())
        .unwrap_or_default()
    {
        if path.is_dir() {
            readonly_paths.push(path);
        }
    }
    readonly_paths.retain(|path| path.is_dir());

    let working_directory = std_cmd
        .get_current_dir()
        .map(PathBuf::from)
        .unwrap_or(std::env::current_dir()?);
    Ok(SandboxedProcessRequest {
        program: std_cmd.get_program().to_os_string(),
        arguments: std_cmd.get_args().map(OsStr::to_os_string).collect(),
        working_directory,
        readwrite_paths,
        readonly_paths,
        environment,
        allow_network,
        allow_desktop_handle_access,
    })
}

#[cfg(windows)]
fn os_key_eq_ignore_ascii_case(key: &std::ffi::OsStr, expected: &str) -> bool {
    key.to_str()
        .is_some_and(|key| key.eq_ignore_ascii_case(expected))
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
    fn workspace_isolation_is_declared_by_the_shell_owner_not_its_name() {
        let policy = ShellTool::default().operation_policy(&json!({}));
        assert_eq!(
            policy.execution_boundary,
            crate::ExecutionBoundary::SandboxedHost
        );

        assert_eq!(
            crate::tool_contract::operation_attributes("shell", crate::ToolConcurrency::Exclusive,)
                .execution_boundary,
            crate::ExecutionBoundary::HostProcess,
            "a tool name alone must not receive the Windows workspace-isolation exemption"
        );
    }

    #[cfg(windows)]
    #[tokio::test]
    #[ignore = "requires a serviced Windows 11 host with MXC BaseContainer support"]
    async fn workspace_write_shell_is_confined_to_configured_roots_by_mxc() {
        use haven_common::types::NetworkPolicy;

        let temp = tempdir().expect("create isolated MXC fixture");
        let workspace_root = temp.path().join("workspace");
        let outside_root = temp.path().join("outside");
        std::fs::create_dir(&workspace_root).expect("create workspace root");
        std::fs::create_dir(&outside_root).expect("create outside root");
        let workspace = ExecutionWorkspace::from_root(&workspace_root).unwrap();
        let (execution_authorization, _) =
            crate::security::ExecutionPolicyAuthorization::for_test_with_workspace_write_root(
                &workspace_root,
                NetworkPolicy::Deny,
            );
        let context = crate::ToolExecutionContext {
            execution_workspace: Some(workspace.clone()),
            execution_authorization: Some(execution_authorization),
            ..Default::default()
        };
        let tool = ShellTool {
            execution_workspace: workspace,
            ..ShellTool::default()
        };
        let result = tool
            .run_with_execution_context(
                ShellParams {
                    command: "echo inside>inside.txt & echo outside>..\\outside\\outside.txt"
                        .into(),
                    shell: Some("cmd".into()),
                    silent: Some(true),
                    cwd: None,
                    step_id: None,
                    session_id: None,
                },
                CancellationToken::new(),
                Some(&context),
            )
            .await
            .expect("MXC must launch the authorized Workspace Write shell");

        assert!(result.output["exit_code"].as_i64().unwrap_or(1) != 0);
        assert_eq!(
            std::fs::read_to_string(workspace_root.join("inside.txt"))
                .unwrap()
                .trim(),
            "inside"
        );
        assert!(
            !outside_root.join("outside.txt").exists(),
            "MXC must deny a write outside the configured workspace root"
        );
    }

    #[cfg(windows)]
    #[tokio::test]
    #[ignore = "run on a serviced Windows host without BaseContainer to exercise AppContainer+DACL"]
    async fn workspace_write_shell_is_confined_by_appcontainer_dacl_compatibility() {
        use haven_common::types::NetworkPolicy;

        let temp = tempdir().expect("create isolated MXC DACL fixture");
        let workspace_root = temp.path().join("workspace");
        let outside_root = temp.path().join("outside");
        std::fs::create_dir(&workspace_root).expect("create workspace root");
        std::fs::create_dir(&outside_root).expect("create outside root");
        let workspace = ExecutionWorkspace::from_root(&workspace_root).unwrap();
        let (execution_authorization, _) =
            crate::security::ExecutionPolicyAuthorization::for_test_with_workspace_write_root(
                &workspace_root,
                NetworkPolicy::Deny,
            );
        let context = crate::ToolExecutionContext {
            execution_workspace: Some(workspace.clone()),
            execution_authorization: Some(execution_authorization),
            ..Default::default()
        };
        let tool = ShellTool {
            execution_workspace: workspace,
            ..ShellTool::default()
        };
        let result = tool
            .run_with_execution_context(
                ShellParams {
                    command: "echo inside>inside.txt & echo outside>..\\outside\\outside.txt"
                        .into(),
                    shell: Some("cmd".into()),
                    silent: Some(true),
                    cwd: None,
                    step_id: None,
                    session_id: None,
                },
                CancellationToken::new(),
                Some(&context),
            )
            .await
            .expect("MXC compatibility tier must launch the authorized Shell");

        assert!(result.output["exit_code"].as_i64().unwrap_or(1) != 0);
        assert_eq!(
            std::fs::read_to_string(workspace_root.join("inside.txt"))
                .unwrap()
                .trim(),
            "inside"
        );
        assert!(
            !outside_root.join("outside.txt").exists(),
            "AppContainer+DACL must deny writes outside the configured workspace root"
        );
        let notice = result.output["sandbox_notice"]
            .as_str()
            .expect("compatibility tier must be reported to the caller");
        assert!(notice.contains("appcontainer-dacl"));
        assert!(notice.contains("ACL"));
    }

    #[cfg(windows)]
    #[tokio::test]
    #[ignore = "requires a serviced Windows 11 host with MXC BaseContainer support"]
    async fn workspace_write_shell_applies_mxc_network_policy_to_host_loopback() {
        use haven_common::types::NetworkPolicy;
        use std::{
            io::Write,
            net::TcpListener,
            sync::mpsc,
            thread,
            time::{Duration, Instant},
        };

        for (network_policy, should_connect) in [
            (NetworkPolicy::Deny, false),
            (NetworkPolicy::Restricted, false),
            (NetworkPolicy::Open, true),
        ] {
            let temp = tempdir().expect("create isolated MXC network fixture");
            let workspace_root = temp.path().join("workspace");
            std::fs::create_dir(&workspace_root).expect("create workspace root");
            let workspace = ExecutionWorkspace::from_root(&workspace_root).unwrap();
            let listener =
                TcpListener::bind(("127.0.0.1", 0)).expect("bind isolated loopback listener");
            let port = listener.local_addr().unwrap().port();
            listener.set_nonblocking(true).unwrap();
            let (accepted_tx, accepted_rx) = mpsc::channel();
            let listener_thread = thread::spawn(move || {
                let deadline = Instant::now() + Duration::from_secs(5);
                loop {
                    match listener.accept() {
                        Ok((mut stream, _)) => {
                            let _ = stream.write_all(b"accepted");
                            let _ = accepted_tx.send(true);
                            return;
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            if Instant::now() >= deadline {
                                break;
                            }
                            thread::sleep(Duration::from_millis(20));
                        }
                        Err(_) => break,
                    }
                }
                let _ = accepted_tx.send(false);
            });

            let script = format!(
                "$client = New-Object Net.Sockets.TcpClient; $connected = $false; try {{ $task = $client.ConnectAsync('127.0.0.1', {port}); $connected = $task.Wait(2000) -and $client.Connected }} catch {{ }}; $client.Close(); if ($connected) {{ exit 0 }} else {{ exit 17 }}"
            );
            let (execution_authorization, _) =
                crate::security::ExecutionPolicyAuthorization::for_test_with_workspace_write_root(
                    &workspace_root,
                    network_policy,
                );
            let context = crate::ToolExecutionContext {
                execution_workspace: Some(workspace.clone()),
                execution_authorization: Some(execution_authorization),
                ..Default::default()
            };
            let tool = ShellTool {
                execution_workspace: workspace,
                ..ShellTool::default()
            };
            let result = tool
                .run_with_execution_context(
                    ShellParams {
                        command: script,
                        shell: Some("powershell".into()),
                        silent: Some(true),
                        cwd: None,
                        step_id: None,
                        session_id: None,
                    },
                    CancellationToken::new(),
                    Some(&context),
                )
                .await
                .expect("MXC must launch the authorized Workspace Write shell");

            let exit_code = result.output["exit_code"].as_i64();
            let host_listener_accepted = accepted_rx
                .recv_timeout(Duration::from_secs(6))
                .expect("loopback listener must report whether a connection arrived");
            listener_thread
                .join()
                .expect("loopback listener thread must not panic");

            assert_eq!(exit_code, Some(if should_connect { 0 } else { 17 }));
            assert_eq!(host_listener_accepted, should_connect);
        }
    }

    #[cfg(windows)]
    async fn workspace_write_shell_descendant_stops_on_cancel_or_revocation(
        revoke_authorization: bool,
    ) {
        use std::{
            path::Path,
            time::{Duration, Instant},
        };

        fn powershell_literal(path: &Path) -> String {
            path.to_string_lossy().replace('\'', "''")
        }

        let temp = tempdir().expect("create isolated MXC process fixture");
        let workspace_root = temp.path().join("workspace");
        std::fs::create_dir(&workspace_root).expect("create workspace root");
        let workspace = ExecutionWorkspace::from_root(&workspace_root).unwrap();
        let pid_file = workspace_root.join("mxc-shell-descendant.pid");
        let error_file = workspace_root.join("mxc-shell-descendant-error.txt");
        let mut descendant =
            crate::process_test_support::DescendantProcessMarker::new(pid_file.clone());
        let marker = powershell_literal(&pid_file);
        let error_marker = powershell_literal(&error_file);
        let command = format!(
            "$ErrorActionPreference = 'Stop'; try {{ $start = New-Object System.Diagnostics.ProcessStartInfo; $start.FileName = [System.IO.Path]::Combine($PSHOME, 'powershell.exe'); $start.Arguments = '-NoProfile -NonInteractive -Command Start-Sleep -Seconds 60'; $start.UseShellExecute = $false; $child = [System.Diagnostics.Process]::Start($start); [System.IO.File]::WriteAllText('{marker}', $child.Id.ToString()) }} catch {{ [System.IO.File]::WriteAllText('{error_marker}', $_.Exception.ToString()); exit 18 }}; Start-Sleep -Seconds 60"
        );

        let (execution_authorization, authorization) =
            crate::security::ExecutionPolicyAuthorization::for_test_with_workspace_write_root(
                &workspace_root,
                haven_common::types::NetworkPolicy::Deny,
            );
        let context = crate::ToolExecutionContext {
            execution_workspace: Some(workspace.clone()),
            execution_authorization: Some(execution_authorization),
            ..Default::default()
        };
        let cancel = CancellationToken::new();
        let run_cancel = cancel.clone();
        let run = tokio::spawn(async move {
            ShellTool {
                execution_workspace: workspace,
                ..ShellTool::default()
            }
            .run_with_execution_context(
                ShellParams {
                    command,
                    shell: Some("powershell".into()),
                    silent: Some(true),
                    cwd: None,
                    step_id: None,
                    session_id: None,
                },
                run_cancel,
                Some(&context),
            )
            .await
        });

        let deadline = Instant::now() + Duration::from_secs(10);
        let pid = loop {
            match descendant.pid() {
                Ok(pid) => break Some(pid),
                Err(_) if Instant::now() < deadline => {
                    tokio::time::sleep(Duration::from_millis(25)).await;
                }
                Err(_) => break None,
            }
        };
        let Some(pid) = pid else {
            cancel.cancel();
            let result = tokio::time::timeout(Duration::from_secs(10), run)
                .await
                .expect("MXC Shell must stop after a missing-descendant probe")
                .expect("MXC Shell task must not panic")
                .expect("MXC Shell adapter must return its process result");
            panic!(
                "MXC Shell must start a descendant before lifecycle interruption; script_error={:?}; shell_result={:?}",
                std::fs::read_to_string(&error_file).ok(),
                result.output
            );
        };

        if revoke_authorization {
            authorization
                .set_execution_mode(haven_common::types::ExecutionMode::Analyze)
                .await;
        } else {
            cancel.cancel();
        }
        let result = tokio::time::timeout(Duration::from_secs(10), run)
            .await
            .expect("MXC Shell lifecycle interruption must complete")
            .expect("MXC Shell task must not panic")
            .expect("MXC Shell adapter must report cancellation as a result");
        assert_eq!(result.outcome, ToolExecutionOutcome::Cancelled);
        descendant
            .wait_for_exit(pid, Duration::from_secs(5))
            .expect("MXC lifecycle interruption must terminate its descendant");
    }

    #[cfg(windows)]
    #[tokio::test]
    #[ignore = "requires a serviced Windows 11 host with MXC BaseContainer support"]
    async fn workspace_write_shell_cancellation_terminates_mxc_descendants() {
        workspace_write_shell_descendant_stops_on_cancel_or_revocation(false).await;
    }

    #[cfg(windows)]
    #[tokio::test]
    #[ignore = "requires a serviced Windows 11 host with MXC BaseContainer support"]
    async fn workspace_write_shell_revocation_terminates_mxc_descendants() {
        workspace_write_shell_descendant_stops_on_cancel_or_revocation(true).await;
    }

    #[test]
    fn test_shell_tool_description() {
        assert!(ShellTool::default().description().contains("shell command"));
    }

    #[test]
    fn shell_authorization_declares_only_an_explicit_working_directory() {
        let tool = ShellTool::default();
        assert!(
            tool.authorization_resources(&serde_json::json!({
                "command": "Get-ChildItem",
                "path": "ignored.txt"
            }))
            .unwrap()
            .is_empty()
        );
        assert_eq!(
            tool.authorization_resources(&serde_json::json!({
                "command": "Get-ChildItem",
                "cwd": "C:\\Projects\\demo"
            }))
            .unwrap(),
            vec![crate::security::DeclaredResource::LocalPath {
                path: PathBuf::from("C:\\Projects\\demo"),
                access: crate::security::LocalPathAccess::Read,
            }]
        );
    }

    #[test]
    fn shell_default_cwd_uses_execution_workspace_and_keeps_explicit_override() {
        let temp = tempdir().unwrap();
        let workspace = ExecutionWorkspace::from_root(temp.path()).unwrap();
        let workspace_root = workspace.root().unwrap().to_path_buf();
        let tool = ShellTool {
            execution_workspace: workspace,
            ..ShellTool::default()
        };

        let default = tool.resolve_shell_context(None, None, None);
        assert_eq!(default.working_directory, Some(workspace_root));

        let replacement = tempdir().unwrap();
        let replacement_workspace = ExecutionWorkspace::from_root(replacement.path()).unwrap();
        let replacement_snapshot = replacement_workspace.snapshot();
        let switched = tool.resolve_shell_context(None, None, Some(&replacement_snapshot));
        assert_eq!(
            switched.working_directory,
            replacement_snapshot.root.clone()
        );

        let explicit = temp.path().join("explicit");
        let selected =
            tool.resolve_shell_context(None, explicit.to_str(), Some(&replacement_snapshot));
        assert_eq!(selected.working_directory, Some(explicit));
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

    #[cfg(windows)]
    #[tokio::test]
    async fn cancelling_shell_terminates_its_descendant_processes() {
        use std::{
            path::Path,
            time::{Duration, Instant},
        };

        fn powershell_literal(path: &Path) -> String {
            path.to_string_lossy().replace('\'', "''")
        }

        let temp = tempdir().expect("create isolated process marker directory");
        let pid_file = temp.path().join("shell-descendant.pid");
        let mut descendant =
            crate::process_test_support::DescendantProcessMarker::new(pid_file.clone());
        let marker = powershell_literal(&pid_file);
        let command = format!(
            "$child = Start-Process -FilePath (Join-Path $PSHOME 'powershell.exe') -ArgumentList '-NoProfile -NonInteractive -Command Start-Sleep -Seconds 60' -PassThru; [System.IO.File]::WriteAllText('{marker}', [string]$child.Id); Start-Sleep -Seconds 60"
        );

        let cancel = CancellationToken::new();
        let run_cancel = cancel.clone();
        let run = tokio::spawn(async move {
            ShellTool::default()
                .execute(
                    json!({"command": command, "shell": "powershell"}),
                    run_cancel,
                )
                .await
        });
        let deadline = Instant::now() + Duration::from_secs(10);
        while !pid_file.exists() && Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        let pid = descendant
            .pid()
            .expect("Shell child must start a descendant before cancellation");

        cancel.cancel();
        let result = tokio::time::timeout(Duration::from_secs(10), run)
            .await
            .expect("Shell cancellation must complete")
            .expect("Shell task must not panic")
            .expect("Shell adapter must report cancellation as a result");
        assert_eq!(result.outcome, ToolExecutionOutcome::Cancelled);
        descendant
            .wait_for_exit(pid, Duration::from_secs(5))
            .expect("Shell cancellation must close the Job Object");
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn policy_revocation_terminates_shell_descendants() {
        use std::{
            path::Path,
            time::{Duration, Instant},
        };

        fn powershell_literal(path: &Path) -> String {
            path.to_string_lossy().replace('\'', "''")
        }

        let temp = tempdir().expect("create isolated process marker directory");
        let pid_file = temp.path().join("revoked-shell-descendant.pid");
        let mut descendant =
            crate::process_test_support::DescendantProcessMarker::new(pid_file.clone());
        let marker = powershell_literal(&pid_file);
        let command = format!(
            "$child = Start-Process -FilePath (Join-Path $PSHOME 'powershell.exe') -ArgumentList '-NoProfile -NonInteractive -Command Start-Sleep -Seconds 60' -PassThru; [System.IO.File]::WriteAllText('{marker}', [string]$child.Id); Start-Sleep -Seconds 60"
        );

        let (execution_authorization, authorization) =
            crate::security::ExecutionPolicyAuthorization::for_test_with_engine_and_sandbox_mode(
                haven_common::types::SandboxMode::FullAccess,
            );
        let context = crate::ToolExecutionContext {
            execution_workspace: None,
            file_write_policy: None,
            execution_authorization: Some(execution_authorization),
            ..Default::default()
        };
        let run = tokio::spawn(async move {
            ShellTool::default()
                .run_with_execution_context(
                    ShellParams {
                        command,
                        shell: Some("powershell".into()),
                        silent: Some(true),
                        cwd: None,
                        step_id: None,
                        session_id: None,
                    },
                    CancellationToken::new(),
                    Some(&context),
                )
                .await
        });
        let deadline = Instant::now() + Duration::from_secs(10);
        while !pid_file.exists() && Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        let pid = descendant
            .pid()
            .expect("Shell child must start a descendant before revocation");

        authorization
            .set_execution_mode(haven_common::types::ExecutionMode::Analyze)
            .await;
        let result = tokio::time::timeout(Duration::from_secs(10), run)
            .await
            .expect("policy revocation must stop the Shell call")
            .expect("Shell task must not panic")
            .expect("Shell adapter must return a cancellation result");
        assert_eq!(result.outcome, ToolExecutionOutcome::Cancelled);
        descendant
            .wait_for_exit(pid, Duration::from_secs(5))
            .expect("policy revocation must close the Job Object");
    }

    #[tokio::test]
    async fn test_shell_timeout_is_unknown_without_replay() {
        #[cfg(windows)]
        let command = "Start-Sleep -Seconds 5";
        #[cfg(not(windows))]
        let command = "sleep 5";
        let (execution_authorization, _) =
            crate::security::ExecutionPolicyAuthorization::for_test_with_engine_and_sandbox_mode(
                haven_common::types::SandboxMode::FullAccess,
            );
        let context = crate::ToolExecutionContext {
            execution_workspace: None,
            file_write_policy: None,
            execution_authorization: Some(execution_authorization),
            ..Default::default()
        };
        let result = ShellTool::default()
            .execute_with_authorization_context_timeout(
                json!({"command": command}),
                CancellationToken::new(),
                1,
                None,
                &context,
            )
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
        let mut version_probe = std::process::Command::new("pwsh");
        version_probe.args(["-NoProfile", "-Command", "$PSVersionTable.PSVersion.Major"]);
        let version_probe = {
            let _spawn_guard = haven_platform::process_containment::process_spawn_lock();
            version_probe.spawn()
        }
        .and_then(std::process::Child::wait_with_output);
        if version_probe.map(|o| !o.status.success()).unwrap_or(true) {
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
        let log_root = tempdir().unwrap();
        let tool = ShellTool {
            test_output_log_root: Some(log_root.path().join("shell-logs")),
            ..ShellTool::default()
        };
        let result = tool
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
        let log_root = tempdir().unwrap();
        let tool = ShellTool {
            test_output_log_root: Some(log_root.path().join("shell-logs")),
            ..ShellTool::default()
        };
        let result = tool
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
        assert!(result.output["log_path"].as_str().is_some());
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
    async fn test_shell_powershell_preserves_nested_quotes_and_plus_expression() {
        let result = ShellTool::default()
            .execute(
                json!({
                    "command": "Write-Output ('left' + \" \" + 'right')",
                    "shell": "powershell"
                }),
                CancellationToken::new(),
            )
            .await
            .unwrap();

        assert!(result.success, "expected success: {:?}", result.error);
        assert_eq!(
            result.output["output"].as_str().unwrap().trim(),
            "left right"
        );
        assert_eq!(result.output["exit_code"], 0);
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

    #[test]
    fn test_shell_schema_has_no_background_mode() {
        let schema = ShellTool::default().input_schema();
        assert!(schema["properties"]["execution_mode"].is_null());
    }

    #[tokio::test]
    async fn shell_rejects_legacy_background_mode_parameter() {
        let tool = ShellTool::default();
        assert!(
            tool.validate_input(&json!({
                "command": "echo legacy",
                "execution_mode": "background"
            }))
            .is_err()
        );
    }
}
