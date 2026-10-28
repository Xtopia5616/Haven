use crate::process::read_stream_text_capped;
use crate::security::{AuthorizationEngine, DeclaredResource, LocalPathAccess};
use crate::tool_contract::parse_tool_input;
use crate::{
    ExecutionBoundary, NetworkAccess, OperationIdempotency, OperationPolicy, Tool, ToolConcurrency,
    ToolExecutionContext, ToolExecutionOutcome, ToolOperationScope, ToolResult,
};
use async_trait::async_trait;
use haven_common::tools::ToolCatalogGroup;
use haven_common::types::{RiskLevel, SandboxMode};
use haven_platform::filesystem::DirectoryCapability;
use serde::Deserialize;
use serde_json::{Value, json};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::Arc;
use tokio::io::AsyncRead;
use tokio::process::Child;
use tokio_util::sync::CancellationToken;

const MAX_GIT_ARGUMENTS: usize = 256;
const MAX_GIT_ARGUMENT_BYTES: usize = 64 * 1024;
const MAX_GIT_ARGUMENT_LENGTH: usize = 4096;
const SCRATCH_CHECK_INTERVAL: std::time::Duration = std::time::Duration::from_secs(1);

const GIT_SUBCOMMANDS: &[&str] = &[
    "add",
    "branch",
    "checkout",
    "cherry-pick",
    "commit",
    "config",
    "diff",
    "fetch",
    "log",
    "ls-files",
    "ls-remote",
    "merge",
    "pull",
    "push",
    "reset",
    "restore",
    "rebase",
    "show",
    "stash",
    "status",
    "switch",
    "tag",
];

const NETWORK_GIT_SUBCOMMANDS: &[&str] = &["fetch", "ls-remote", "pull", "push"];

const FORBIDDEN_GIT_ARGUMENTS: &[&str] = &[
    "-C",
    "-c",
    "--config-env",
    "--exec-path",
    "--git-dir",
    "--super-prefix",
    "--work-tree",
];

pub(crate) struct GitTool {
    authorization: Arc<AuthorizationEngine>,
    max_output_chars: usize,
}

impl GitTool {
    pub(crate) fn new(authorization: Arc<AuthorizationEngine>, max_output_chars: usize) -> Self {
        Self {
            authorization,
            max_output_chars,
        }
    }

    fn policy_for(&self, input: &Value) -> Result<OperationPolicy, String> {
        let params = parse_git_params(input)?;
        validate_git_params(&params)?;
        let mut policy = self.base_policy();
        if is_network_subcommand(&params.subcommand) {
            policy.network_access = NetworkAccess::Opaque;
        }
        Ok(policy)
    }

    fn base_policy(&self) -> OperationPolicy {
        self.operation_spec()
            .expect("Git tool declares its operation policy")
            .policy
    }

    async fn run(
        &self,
        input: Value,
        cancel: CancellationToken,
        context: &ToolExecutionContext,
    ) -> anyhow::Result<ToolResult> {
        let params = parse_tool_input::<GitParams>("git", input)?;
        let network_required = is_network_subcommand(&params.subcommand);
        let sandbox_mode = self.authorization.sandbox_mode().await;
        let opaque_network_required = network_required || sandbox_mode == SandboxMode::FullAccess;
        validate_git_params(&params).map_err(anyhow::Error::msg)?;
        let authorization = context.clone_execution_authorization().ok_or_else(|| {
            crate::security::execution_policy_denied(
                "Git process requires proof-derived execution authorization".into(),
            )
        })?;
        let permit = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Ok(ToolResult::cancelled("Git operation cancelled before process start")),
            result = context.acquire_git_process_start_permit(opaque_network_required) => {
                result.map_err(crate::security::execution_policy_denied)?
            }
        };
        if cancel.is_cancelled() {
            return Ok(ToolResult::cancelled(
                "Git operation cancelled before process start",
            ));
        }

        let workspace_root = context
            .execution_workspace
            .as_ref()
            .and_then(|workspace| workspace.root())
            .map(PathBuf::from)
            .ok_or_else(|| anyhow::anyhow!("Git execution workspace is unavailable"))?;
        let workspace = DirectoryCapability::open(&workspace_root)
            .map_err(|_| anyhow::anyhow!("Git execution workspace is unavailable or unsafe"))?;
        let working_directory = resolve_working_directory(&params.cwd, &workspace)?;
        check_working_directory(
            &working_directory,
            &workspace,
            permit.authorizes_git_directory(&working_directory),
        )?;
        let repository = discover_git_repository(&working_directory)?;
        let mut repository_directories = Vec::new();
        for path in [
            &repository.worktree,
            &repository.metadata,
            &repository.common,
        ] {
            let directory = DirectoryCapability::open(path).map_err(|_| {
                anyhow::anyhow!("Git repository directory is unavailable or unsafe")
            })?;
            anyhow::ensure!(
                permit.authorizes_git_directory(&directory),
                "Git repository changed after authorization"
            );
            repository_directories.push(directory);
        }
        if cancel.is_cancelled() {
            return Ok(ToolResult::cancelled(
                "Git operation cancelled before process start",
            ));
        }
        #[cfg(windows)]
        let process_network_policy = permit.process_network_policy();
        let git_metadata = vec![repository.metadata.clone(), repository.common.clone()];
        let scratch = crate::shell_runtime::ProcessScratchDirectory::create_git().await?;
        scratch.ensure_within_capacity().await?;
        let global_git_config = scratch.path().join("global.gitconfig");
        std::fs::File::create(&global_git_config)?;
        let mut command = Command::new("git");
        command
            .arg(&params.subcommand)
            .args(&params.args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .current_dir(working_directory.root())
            .env_clear()
            .envs(
                haven_platform::process_environment::filter_runtime_baseline_environment(
                    std::env::vars_os(),
                ),
            )
            .env("TEMP", scratch.path())
            .env("TMP", scratch.path())
            .env("TMPDIR", scratch.path())
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", &global_git_config)
            .env("GIT_TERMINAL_PROMPT", "0");

        if cancel.is_cancelled() {
            return Ok(ToolResult::cancelled(
                "Git operation cancelled before process start",
            ));
        }
        let (mut process, sandbox_notice) = match permit.sandbox_mode() {
            SandboxMode::FullAccess => (spawn_host_git(command).await?, None),
            SandboxMode::WorkspaceWrite => {
                #[cfg(windows)]
                {
                    use haven_platform::sandboxed_process::SandboxedProcessNetworkPolicy;

                    let runtime_roots = crate::process_sandbox::runtime_dependency_roots(&command);
                    let filesystem = permit
                        .workspace_git_directories(
                            working_directory.root(),
                            &runtime_roots,
                            scratch.path(),
                            &git_metadata,
                        )
                        .map_err(crate::security::execution_policy_denied)?;
                    let request = crate::process_sandbox::request_from_command(
                        &command,
                        filesystem
                            .readwrite_roots
                            .iter()
                            .map(|root| root.root().to_path_buf())
                            .collect(),
                        filesystem
                            .readonly_roots
                            .iter()
                            .map(|root| root.root().to_path_buf())
                            .collect(),
                        filesystem.protected_readonly_paths,
                        filesystem.denied_paths,
                        if network_required {
                            process_network_policy
                        } else {
                            SandboxedProcessNetworkPolicy::default()
                        },
                        permit.authorizes_git_desktop_handles(),
                    )?;
                    let mut process = haven_platform::sandboxed_process::SandboxedProcess::spawn(
                        request,
                        permit.mxc_isolation_mode(),
                    )
                    .await?;
                    let notice = process.sandbox_notice().to_owned();
                    let stdout = process
                        .take_stdout()
                        .map(|stream| Box::new(stream) as Box<dyn AsyncRead + Unpin + Send>);
                    let stderr = process
                        .take_stderr()
                        .map(|stream| Box::new(stream) as Box<dyn AsyncRead + Unpin + Send>);
                    (GitProcess::Sandboxed(process, stdout, stderr), Some(notice))
                }
                #[cfg(not(windows))]
                {
                    let _ = (git_metadata, permit, command);
                    anyhow::bail!("Workspace Write Git operations require Windows MXC isolation")
                }
            }
            SandboxMode::ReadOnly => {
                anyhow::bail!("Git operations are blocked by the read-only sandbox")
            }
        };
        drop(repository_directories);
        drop(permit);

        let output_cap = crate::collect_byte_cap(self.max_output_chars);
        let capacity_budget = scratch.capacity_budget();
        let capacity_exceeded = async move {
            loop {
                tokio::time::sleep(SCRATCH_CHECK_INTERVAL).await;
                let budget = capacity_budget.clone();
                let exceeded = tokio::task::spawn_blocking(move || budget.check_exceeded()).await;
                match exceeded {
                    Ok(Ok(false)) => {}
                    Ok(Ok(true)) => return "Git scratch exceeded its capacity limit".to_owned(),
                    Ok(Err(error)) => return error,
                    Err(_) => return "Git scratch could not be inspected safely".into(),
                }
            }
        };
        tokio::pin!(capacity_exceeded);
        let command_result = async { process.collect_output(output_cap).await };
        let policy_revoked = authorization.wait_for_policy_revocation();
        tokio::pin!(policy_revoked);
        let (stdout, stderr, exit_code, sandbox_exit) = tokio::select! {
            result = command_result => {
                result?
            }
            _ = cancel.cancelled() => {
                let _ = process.kill_and_wait().await;
                return Ok(ToolResult::cancelled("Git operation cancelled"));
            }
            _ = &mut policy_revoked => {
                let _ = process.kill_and_wait().await;
                return Ok(ToolResult::cancelled("execution policy changed; Git operation cancelled"));
            }
            reason = &mut capacity_exceeded => {
                let _ = process.kill_and_wait().await;
                return Ok(ToolResult::failed(Value::Null, reason));
            }
        };

        let truncated = stdout.overflowed || stderr.overflowed;
        let mut output = json!({
            "subcommand": params.subcommand,
            "stdout": stdout.text,
            "stderr": stderr.text,
            "exit_code": exit_code,
        });
        if let Some(notice) = sandbox_notice {
            output["sandbox_notice"] = json!(notice);
        }
        if let Some(exit) = sandbox_exit {
            output["sandbox_isolation"] = serde_json::to_value(exit.isolation)?;
        }
        if exit_code == 0 {
            Ok(ToolResult::from_output(output, truncated))
        } else {
            Ok(ToolResult::failed_with_metadata(
                output,
                format!("Git {} exited with status {exit_code}", params.subcommand),
                crate::ToolErrorMetadata::other(),
            ))
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct GitParams {
    subcommand: String,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    cwd: Option<String>,
}

#[derive(Debug)]
struct CappedText {
    text: String,
    overflowed: bool,
}

enum GitProcess {
    Host {
        child: Child,
        _containment: haven_platform::process_containment::ProcessContainment,
        stdout: Option<Box<dyn AsyncRead + Unpin + Send>>,
        stderr: Option<Box<dyn AsyncRead + Unpin + Send>>,
    },
    Sandboxed(
        haven_platform::sandboxed_process::SandboxedProcess,
        Option<Box<dyn AsyncRead + Unpin + Send>>,
        Option<Box<dyn AsyncRead + Unpin + Send>>,
    ),
}

impl GitProcess {
    async fn collect_output(
        &mut self,
        max_bytes: usize,
    ) -> anyhow::Result<(
        CappedText,
        CappedText,
        i32,
        Option<haven_platform::sandboxed_process::SandboxedProcessExit>,
    )> {
        let (stdout, stderr, status) = match self {
            Self::Host {
                child,
                stdout,
                stderr,
                ..
            } => {
                let (stdout, stderr) = tokio::join!(
                    read_stream_text_capped(stdout.take(), max_bytes, None),
                    read_stream_text_capped(stderr.take(), max_bytes, None),
                );
                let status = child.wait().await?;
                (stdout, stderr, (status.code().unwrap_or(-1), None))
            }
            Self::Sandboxed(child, stdout_stream, stderr_stream) => {
                let (stdout, stderr) = tokio::join!(
                    read_stream_text_capped(stdout_stream.take(), max_bytes, None),
                    read_stream_text_capped(stderr_stream.take(), max_bytes, None),
                );
                let exit = child.wait().await?;
                let code = exit.exit_code;
                (stdout, stderr, (code, Some(exit)))
            }
        };
        Ok((
            CappedText {
                text: stdout.text,
                overflowed: stdout.overflowed,
            },
            CappedText {
                text: stderr.text,
                overflowed: stderr.overflowed,
            },
            status.0,
            status.1,
        ))
    }

    async fn kill_and_wait(&mut self) -> anyhow::Result<()> {
        match self {
            Self::Host { child, .. } => {
                let _ = child.kill().await;
                let _ = child.wait().await?;
            }
            Self::Sandboxed(child, ..) => {
                let _ = child.kill().await;
                let _ = child.wait().await?;
            }
        }
        Ok(())
    }
}

async fn spawn_host_git(command: Command) -> anyhow::Result<GitProcess> {
    let containment = haven_platform::process_containment::ProcessContainment::new()?;
    let mut child_command = tokio::process::Command::from(command);
    #[cfg(windows)]
    containment.prepare_command(child_command.as_std_mut(), crate::CREATE_NO_WINDOW);
    #[cfg(not(windows))]
    containment.prepare_command(child_command.as_std_mut(), 0);
    let spawn = {
        let _spawn_guard = haven_platform::process_containment::process_spawn_lock();
        child_command.kill_on_drop(true).spawn()
    };
    let mut child = spawn.map_err(|_| anyhow::anyhow!("could not start the Git executable"))?;
    let pid = child
        .id()
        .ok_or_else(|| anyhow::anyhow!("Git process did not expose a process id"))?;
    #[cfg(windows)]
    let attach = child
        .raw_handle()
        .ok_or_else(|| std::io::Error::other("Git process handle is unavailable"))
        .and_then(|handle| containment.attach_and_resume(pid, handle));
    #[cfg(not(windows))]
    let attach = containment.attach_and_resume(pid, ());
    if attach.is_err() {
        let _ = child.kill().await;
        let _ = child.wait().await;
        anyhow::bail!("could not contain the Git process tree");
    }
    let stdout = child
        .stdout
        .take()
        .map(|stream| Box::new(stream) as Box<dyn AsyncRead + Unpin + Send>);
    let stderr = child
        .stderr
        .take()
        .map(|stream| Box::new(stream) as Box<dyn AsyncRead + Unpin + Send>);
    Ok(GitProcess::Host {
        child,
        _containment: containment,
        stdout,
        stderr,
    })
}

fn parse_git_params(input: &Value) -> Result<GitParams, String> {
    parse_tool_input::<GitParams>("git", input.clone()).map_err(|error| error.to_string())
}

fn validate_git_params(params: &GitParams) -> Result<(), String> {
    if !GIT_SUBCOMMANDS.contains(&params.subcommand.as_str()) {
        return Err("Git subcommand is unsupported".into());
    }
    if params.args.len() > MAX_GIT_ARGUMENTS {
        return Err("Git argument list exceeds the supported limit".into());
    }
    let mut total_bytes = 0usize;
    for argument in &params.args {
        if argument.len() > MAX_GIT_ARGUMENT_LENGTH || argument.contains('\0') {
            return Err("Git argument is invalid or too long".into());
        }
        total_bytes = total_bytes.saturating_add(argument.len());
        let normalized = argument.to_ascii_lowercase();
        if FORBIDDEN_GIT_ARGUMENTS.contains(&normalized.as_str())
            || [
                "--git-dir=",
                "--work-tree=",
                "--exec-path=",
                "--config-env=",
                "--super-prefix=",
            ]
            .iter()
            .any(|prefix| normalized.starts_with(prefix))
        {
            return Err(
                "Git arguments cannot override the authorized repository or process scope".into(),
            );
        }
    }
    if total_bytes > MAX_GIT_ARGUMENT_BYTES {
        return Err("Git argument list exceeds the supported byte limit".into());
    }
    if params
        .cwd
        .as_ref()
        .is_some_and(|cwd| cwd.trim().is_empty() || cwd.contains('\0'))
    {
        return Err("Git working directory is invalid".into());
    }
    if params.subcommand == "config" {
        validate_local_config_arguments(&params.args)?;
    }
    Ok(())
}

fn validate_local_config_arguments(arguments: &[String]) -> Result<(), String> {
    let valid_key = |key: &str| matches!(key, "user.name" | "user.email");
    let valid = match arguments {
        [scope, mode, key] if scope == "--local" && (mode == "--get" || mode == "--get-all") => {
            valid_key(key)
        }
        [scope, key, _value] if scope == "--local" => valid_key(key),
        [scope, mode, key, _value] if scope == "--local" && mode == "--replace-all" => {
            valid_key(key)
        }
        [scope, mode, key]
            if scope == "--local" && (mode == "--unset" || mode == "--unset-all") =>
        {
            valid_key(key)
        }
        _ => false,
    };
    if valid {
        Ok(())
    } else {
        Err("Git config only supports --local user.name and user.email values".into())
    }
}

fn is_network_subcommand(subcommand: &str) -> bool {
    NETWORK_GIT_SUBCOMMANDS.contains(&subcommand)
}

fn resolve_working_directory(
    cwd: &Option<String>,
    workspace: &DirectoryCapability,
) -> anyhow::Result<DirectoryCapability> {
    let requested = cwd.as_ref().map_or_else(
        || workspace.root().to_path_buf(),
        |cwd| {
            let path = PathBuf::from(cwd);
            if path.is_absolute() {
                path
            } else {
                workspace.root().join(path)
            }
        },
    );
    let directory = DirectoryCapability::open(&requested)
        .map_err(|_| anyhow::anyhow!("Git working directory is unavailable or unsafe"))?;
    Ok(directory)
}

fn check_working_directory(
    directory: &DirectoryCapability,
    workspace: &DirectoryCapability,
    authorized: bool,
) -> anyhow::Result<()> {
    anyhow::ensure!(
        directory_contains_path(workspace, directory.root())? || authorized,
        "Git working directory is outside the authorized scope"
    );
    Ok(())
}

#[derive(Debug, PartialEq, Eq)]
struct GitRepository {
    worktree: PathBuf,
    metadata: PathBuf,
    common: PathBuf,
}

pub(crate) fn authorization_resources_for_workspace(
    input: &Value,
    workspace_root: &std::path::Path,
) -> Result<Vec<DeclaredResource>, String> {
    let params = parse_git_params(input)?;
    validate_git_params(&params)?;
    let workspace = DirectoryCapability::open(workspace_root)
        .map_err(|_| "Git workspace is unavailable or unsafe".to_owned())?;
    let cwd =
        resolve_working_directory(&params.cwd, &workspace).map_err(|error| error.to_string())?;
    let repository = discover_git_repository(&cwd).map_err(|error| error.to_string())?;
    let mut paths = vec![
        cwd.root().to_path_buf(),
        repository.worktree,
        repository.metadata,
        repository.common,
    ];
    paths.sort();
    paths.dedup();
    let mut resources: Vec<_> = paths
        .into_iter()
        .map(|path| DeclaredResource::LocalPath {
            path,
            access: LocalPathAccess::Write,
        })
        .collect();
    if cfg!(windows) {
        resources.push(DeclaredResource::Desktop {
            access: haven_common::types::DesktopAccess::ProcessHandles,
        });
    }
    Ok(resources)
}

fn read_git_pointer(directory: &DirectoryCapability, name: &str) -> anyhow::Result<String> {
    use std::io::Read;
    let file = directory.open_read(std::path::Path::new(name))?;
    let mut bytes = Vec::new();
    file.take(8193).read_to_end(&mut bytes)?;
    anyhow::ensure!(bytes.len() <= 8192, "Git metadata pointer is too long");
    let text = String::from_utf8(bytes)?;
    let text = text.trim_end_matches(['\r', '\n']);
    anyhow::ensure!(
        !text.is_empty() && !text.contains(['\r', '\n', '\0']),
        "invalid Git metadata pointer"
    );
    Ok(text.to_owned())
}

fn discover_git_repository(cwd: &DirectoryCapability) -> anyhow::Result<GitRepository> {
    let mut candidate = cwd.root().to_path_buf();
    loop {
        let directory = DirectoryCapability::open(&candidate)?;
        let marker = candidate.join(".git");
        match std::fs::symlink_metadata(&marker) {
            Ok(info) if info.is_dir() => {
                let metadata_directory = DirectoryCapability::open(&marker)?;
                anyhow::ensure!(
                    !directory_contains_path(&metadata_directory, cwd.root())?,
                    "Git working directory cannot be inside the metadata directory"
                );
                let metadata = metadata_directory.root().to_path_buf();
                return Ok(GitRepository {
                    worktree: candidate,
                    common: metadata.clone(),
                    metadata,
                });
            }
            Ok(info) if info.is_file() => {
                let text = read_git_pointer(&directory, ".git")?;
                let target = text
                    .strip_prefix("gitdir: ")
                    .ok_or_else(|| anyhow::anyhow!("invalid Git worktree metadata pointer"))?;
                let metadata = DirectoryCapability::open(&candidate.join(target))?;
                anyhow::ensure!(
                    !directory_contains_path(&metadata, cwd.root())?,
                    "Git working directory cannot be inside the metadata directory"
                );
                let common = match read_git_pointer(&metadata, "commondir") {
                    Ok(target) => DirectoryCapability::open(&metadata.root().join(target))?
                        .root()
                        .to_path_buf(),
                    Err(error)
                        if error
                            .downcast_ref::<std::io::Error>()
                            .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound) =>
                    {
                        metadata.root().to_path_buf()
                    }
                    Err(error) => return Err(error),
                };
                return Ok(GitRepository {
                    worktree: candidate,
                    metadata: metadata.root().to_path_buf(),
                    common,
                });
            }
            Ok(_) => anyhow::bail!("Git metadata marker is unsafe"),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        if !candidate.pop() {
            break;
        }
    }
    anyhow::bail!("Git repository not found")
}

fn directory_contains_path(
    root: &DirectoryCapability,
    path: &std::path::Path,
) -> std::io::Result<bool> {
    haven_platform::filesystem::FilesystemPathCapability::open(root.root())?.contains_path(path)
}

#[async_trait]
impl Tool for GitTool {
    fn name(&self) -> String {
        "git".into()
    }

    fn description(&self) -> String {
        "Run one allowlisted Git subcommand directly inside the current repository. WorkspaceWrite isolates the Git process tree and grants repository metadata writes only to this process family; hooks and filters inherit the same MXC restrictions.".into()
    }

    fn catalog_group(&self) -> ToolCatalogGroup {
        ToolCatalogGroup::Shell
    }

    fn operation_spec(&self) -> Option<crate::operation_view::OperationSpec> {
        let mut spec = crate::operation_view::root_tool_spec(
            "git",
            "Run a scoped Git command in the current repository.",
            self.input_schema(),
            RiskLevel::High,
            OperationIdempotency::Unknown,
            ToolOperationScope::Session,
            ToolConcurrency::Exclusive,
            ToolCatalogGroup::Shell,
            None,
        );
        spec.policy.network_access = NetworkAccess::None;
        spec.policy.execution_boundary = ExecutionBoundary::SandboxedHost;
        spec.policy.effect = crate::OperationEffect::WorkspaceWrite;
        spec.policy.data_sensitivity = crate::DataSensitivity::UserData;
        Some(spec)
    }

    fn try_operation_policy(&self, input: &Value) -> Result<OperationPolicy, String> {
        self.policy_for(input)
    }

    fn risk_level(&self, _input: &Value) -> RiskLevel {
        RiskLevel::High
    }

    fn authorization_resources(&self, input: &Value) -> Result<Vec<DeclaredResource>, String> {
        let params = parse_git_params(input)?;
        validate_git_params(&params)?;
        Ok(vec![DeclaredResource::LocalPath {
            path: params
                .cwd
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from(".")),
            access: LocalPathAccess::Write,
        }])
    }

    async fn authorization_contract(
        &self,
        input: &Value,
    ) -> Result<crate::ToolAuthorizationContract, String> {
        let mut policy = self.policy_for(input)?;
        match self.authorization.sandbox_mode().await {
            SandboxMode::FullAccess => {
                policy.execution_boundary = ExecutionBoundary::HostProcess;
                policy.network_access = NetworkAccess::Opaque;
            }
            SandboxMode::WorkspaceWrite => {
                policy.execution_boundary = ExecutionBoundary::SandboxedHost;
                if is_network_subcommand(&parse_git_params(input)?.subcommand) {
                    policy.network_access = NetworkAccess::Opaque;
                } else {
                    policy.network_access = NetworkAccess::None;
                }
            }
            SandboxMode::ReadOnly => {
                policy.execution_boundary = ExecutionBoundary::HostProcess;
                policy.network_access = NetworkAccess::Opaque;
            }
        }
        Ok(crate::ToolAuthorizationContract {
            policy,
            target_binding: None,
            resources: self.authorization_resources(input)?,
        })
    }

    fn timeout_outcome(&self) -> ToolExecutionOutcome {
        ToolExecutionOutcome::TimedOutUnknown
    }

    fn default_timeout_secs(&self) -> u64 {
        300
    }

    fn input_schema(&self) -> Value {
        json!({
            "type":"object",
            "additionalProperties":false,
            "properties": {
                "subcommand": {"type":"string", "enum": GIT_SUBCOMMANDS},
                "args": {"type":"array", "maxItems":MAX_GIT_ARGUMENTS, "items":{"type":"string", "maxLength":MAX_GIT_ARGUMENT_LENGTH}},
                "cwd": {"type":"string", "minLength":1, "maxLength":4096, "description":"Optional working directory. Defaults to the active execution workspace root."}
            },
            "required":["subcommand"]
        })
    }

    async fn execute(
        &self,
        _input: Value,
        _cancel: CancellationToken,
    ) -> anyhow::Result<ToolResult> {
        anyhow::bail!("Git execution requires proof-derived process authorization")
    }

    async fn execute_with_authorization_context(
        &self,
        input: Value,
        cancel: CancellationToken,
        target_binding: Option<&str>,
        context: &ToolExecutionContext,
    ) -> anyhow::Result<ToolResult> {
        if target_binding.is_some() {
            anyhow::bail!("Git tool cannot enforce an external authorization target");
        }
        self.run(input, cancel, context).await
    }
}

#[cfg(test)]
mod tests {
    use super::{GitParams, GitTool, LocalPathAccess, validate_git_params};
    use crate::{NetworkAccess, Tool};
    use serde_json::json;
    use std::sync::Arc;

    #[tokio::test]
    async fn git_network_access_matches_process_boundary() {
        let tool = GitTool::new(Arc::new(crate::security::AuthorizationEngine::new()), 8_000);
        tool.authorization
            .set_boundaries(
                haven_common::types::SandboxMode::WorkspaceWrite,
                Vec::new(),
                haven_common::types::NetworkPolicy::Deny,
            )
            .await;
        let local = tool
            .authorization_contract(&json!({"subcommand":"status"}))
            .await
            .expect("local WorkspaceWrite Git contract resolves");
        let remote = tool
            .authorization_contract(&json!({"subcommand":"push", "args":["origin", "HEAD"]}))
            .await
            .expect("remote WorkspaceWrite Git contract resolves");

        assert_eq!(local.policy.network_access, NetworkAccess::None);
        assert_eq!(remote.policy.network_access, NetworkAccess::Opaque);

        tool.authorization
            .set_boundaries(
                haven_common::types::SandboxMode::FullAccess,
                Vec::new(),
                haven_common::types::NetworkPolicy::Open,
            )
            .await;
        let local_full_access = tool
            .authorization_contract(&json!({"subcommand":"status"}))
            .await
            .expect("FullAccess Git contract resolves");
        assert_eq!(
            local_full_access.policy.network_access,
            NetworkAccess::Opaque
        );
    }

    #[test]
    fn git_rejects_working_directory_inside_metadata() {
        let root = tempfile::tempdir().unwrap();
        let metadata = root.path().join(".git/objects");
        std::fs::create_dir_all(&metadata).unwrap();
        let cwd = haven_platform::filesystem::DirectoryCapability::open(&metadata).unwrap();
        assert!(super::discover_git_repository(&cwd).is_err());
    }

    #[test]
    fn git_rejects_malformed_and_oversized_worktree_pointers() {
        let root = tempfile::tempdir().unwrap();
        let cwd = haven_platform::filesystem::DirectoryCapability::open(root.path()).unwrap();
        std::fs::write(root.path().join(".git"), "gitdir: bad\nextra\n").unwrap();
        assert!(super::discover_git_repository(&cwd).is_err());
        std::fs::write(root.path().join(".git"), vec![b'x'; 8193]).unwrap();
        assert!(super::discover_git_repository(&cwd).is_err());
    }

    #[test]
    fn git_discovers_linked_worktree_and_common_metadata() {
        let root = tempfile::tempdir().unwrap();
        let worktree = root.path().join("worktree");
        let common = root.path().join("repository/.git");
        let metadata = common.join("worktrees/linked");
        std::fs::create_dir_all(worktree.join("src")).unwrap();
        std::fs::create_dir_all(&metadata).unwrap();
        std::fs::write(
            worktree.join(".git"),
            format!("gitdir: {}\n", metadata.display()),
        )
        .unwrap();
        std::fs::write(metadata.join("commondir"), "../..\n").unwrap();
        let cwd =
            haven_platform::filesystem::DirectoryCapability::open(&worktree.join("src")).unwrap();
        let found = super::discover_git_repository(&cwd).unwrap();
        assert_eq!(found.worktree, worktree);
        assert_eq!(found.metadata, metadata);
        assert_eq!(found.common, common);
        let resources = super::authorization_resources_for_workspace(
            &json!({"subcommand":"status", "cwd":worktree.join("src")}),
            root.path(),
        )
        .unwrap();
        for expected in [
            worktree.join("src"),
            worktree.clone(),
            metadata.clone(),
            common.clone(),
        ] {
            assert!(resources.iter().any(|resource| matches!(resource, crate::DeclaredResource::LocalPath { path, access: LocalPathAccess::Write } if path == &expected)));
        }
    }

    #[test]
    fn git_allows_only_the_declared_external_working_directory() {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("workspace");
        let external = root.path().join("external");
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::create_dir_all(&external).unwrap();
        let workspace = haven_platform::filesystem::DirectoryCapability::open(&workspace).unwrap();
        let cwd = Some(external.to_string_lossy().into_owned());
        let directory = super::resolve_working_directory(&cwd, &workspace).unwrap();
        assert!(super::check_working_directory(&directory, &workspace, false).is_err());
        assert!(super::check_working_directory(&directory, &workspace, true).is_ok());
    }

    #[test]
    fn git_rejects_scope_overrides_and_non_builtin_subcommands() {
        let args = GitParams {
            subcommand: "status".into(),
            args: vec!["--git-dir=C:\\outside\\.git".into()],
            cwd: None,
        };
        assert!(validate_git_params(&args).is_err());

        let subcommand = GitParams {
            subcommand: "config".into(),
            args: Vec::new(),
            cwd: None,
        };
        assert!(validate_git_params(&subcommand).is_err());

        let global_config = GitParams {
            subcommand: "config".into(),
            args: vec!["--global".into(), "user.name".into(), "Alice".into()],
            cwd: None,
        };
        assert!(validate_git_params(&global_config).is_err());

        let local_identity = GitParams {
            subcommand: "config".into(),
            args: vec!["--local".into(), "user.name".into(), "Alice".into()],
            cwd: None,
        };
        assert!(validate_git_params(&local_identity).is_ok());

        let arbitrary_config = GitParams {
            subcommand: "config".into(),
            args: vec!["--local".into(), "core.pager".into(), "cmd".into()],
            cwd: None,
        };
        assert!(validate_git_params(&arbitrary_config).is_err());
    }

    #[test]
    fn git_always_declares_a_write_resource_for_its_effective_working_directory() {
        let tool = GitTool::new(Arc::new(crate::security::AuthorizationEngine::new()), 8_000);
        let spec = tool
            .operation_spec()
            .expect("Git operation contract exists");
        assert_eq!(spec.policy.effect, crate::OperationEffect::WorkspaceWrite);
        assert_eq!(
            spec.policy.data_sensitivity,
            crate::DataSensitivity::UserData
        );
        let resources = tool
            .authorization_resources(&json!({"subcommand":"status"}))
            .expect("default cwd resource is valid");
        assert!(matches!(
            resources.as_slice(),
            [crate::security::DeclaredResource::LocalPath {
                path,
                access: LocalPathAccess::Write,
            }] if path == std::path::Path::new(".")
        ));
    }
}
