//! User-configured lifecycle command execution.
//!
//! FullAccess hooks run as host commands only with NetworkPolicy::Open;
//! WorkspaceWrite hooks use Windows MXC. Job Object containment manages the
//! host process tree but does not itself sandbox filesystem or network access.

use crate::security::{
    AuthorizationEngine, LifecycleHookProcessStartAuthorization, LifecycleHookProcessStartRequest,
};
use haven_common::config::{
    ConfigService, HookCommand, LifecycleHooksConfig, MAX_HOOKS_PER_EVENT, parse_project_hooks,
};
use haven_common::hooks::{
    LifecycleHookDecision, LifecycleHookEvent, LifecycleHookEventDiagnostics, LifecycleHookOutcome,
    LifecycleHookPayload,
};
use haven_platform::filesystem::DirectoryCapability;
use haven_platform::process_containment::{ProcessContainment, process_spawn_lock};
use serde::Deserialize;
use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio_util::sync::CancellationToken;

const PROJECT_HOOKS_PATH: &str = ".haven/hooks.toml";
const MAX_PROJECT_CONFIG_BYTES: usize = 64 * 1024;
const MAX_HOOK_PAYLOAD_BYTES: usize = 64 * 1024;
const MAX_HOOK_OUTPUT_BYTES: usize = 64 * 1024;
const MAX_HOOK_EVENT_DURATION: Duration = Duration::from_secs(5);
const MAX_HOOK_PROCESS_DURATION: Duration = Duration::from_secs(5);
const PROCESS_CLEANUP_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Default)]
pub(crate) struct LifecycleHookService {
    config_service: OnceLock<Arc<ConfigService>>,
    diagnostics: Mutex<HashMap<LifecycleHookEvent, LifecycleHookEventDiagnostics>>,
}

impl LifecycleHookService {
    pub(crate) fn bind_config_service(
        &self,
        config_service: Option<Arc<ConfigService>>,
    ) -> anyhow::Result<()> {
        if let Some(config_service) = config_service {
            self.config_service
                .set(config_service)
                .map_err(|_| anyhow::anyhow!("lifecycle hook config source is already bound"))?;
        }
        Ok(())
    }

    pub(crate) async fn run(
        &self,
        authorization: &Arc<AuthorizationEngine>,
        mut payload: LifecycleHookPayload,
        cancel: CancellationToken,
    ) -> LifecycleHookDecision {
        self.begin_event(payload.event);
        let Some(config_service) = self.config_service.get() else {
            self.finish_event(payload.event, LifecycleHookOutcome::NoHooks, None);
            return LifecycleHookDecision::Allow;
        };
        let snapshot = match config_service.snapshot() {
            Ok(snapshot) => snapshot,
            Err(_) => {
                return self
                    .failure_decision(payload.event, "configuration_unavailable")
                    .await;
            }
        };

        let workspace = authorization.execution_workspace().await;
        let Some(workspace_root) = workspace.root().map(Path::to_path_buf) else {
            if snapshot
                .config
                .hooks
                .for_event(payload.event)
                .iter()
                .any(|hook| hook.enabled)
            {
                return self
                    .failure_decision(payload.event, "execution_workspace_unavailable")
                    .await;
            }
            self.finish_event(payload.event, LifecycleHookOutcome::NoHooks, None);
            return LifecycleHookDecision::Allow;
        };

        let mut hooks = snapshot.config.hooks.for_event(payload.event).to_vec();
        if is_trusted_workspace(&workspace_root, &snapshot.config.trusted_hook_workspaces) {
            match read_project_hooks(workspace.directory().as_ref()) {
                Ok(Some(project_hooks)) => {
                    hooks.extend(project_hooks.for_event(payload.event).iter().cloned());
                }
                Ok(None) => {}
                Err(_) => {
                    return self
                        .failure_decision(payload.event, "project_configuration_invalid")
                        .await;
                }
            }
        }

        hooks.retain(|hook| hook.enabled && matcher_applies(hook, payload.tool_name.as_deref()));
        if hooks.is_empty() {
            self.finish_event(payload.event, LifecycleHookOutcome::NoHooks, None);
            return LifecycleHookDecision::Allow;
        }
        if hooks.len() > MAX_HOOKS_PER_EVENT * 2 {
            return self
                .failure_decision(payload.event, "too_many_matching_commands")
                .await;
        }

        let hook_authorization = match authorization
            .capture_lifecycle_hook_process_start_authorization()
            .await
        {
            Ok(authorization)
                if authorization.execution_workspace_generation() == workspace.generation() =>
            {
                authorization
            }
            Ok(_) => {
                return self
                    .failure_decision(payload.event, "execution_workspace_changed")
                    .await;
            }
            Err(_) => {
                return self
                    .failure_decision(payload.event, "security_policy_denied")
                    .await;
            }
        };

        let event_deadline = tokio::time::Instant::now() + MAX_HOOK_EVENT_DURATION;
        let mut had_failure = false;
        let mut event_failure_code = None;
        for hook in hooks {
            if cancel.is_cancelled() {
                return self.failure_decision(payload.event, "cancelled").await;
            }
            payload.payload_truncated = false;
            let input = match bounded_payload(&mut payload) {
                Ok(input) => input,
                Err(_) => {
                    return self
                        .failure_decision(payload.event, "payload_exceeds_limit")
                        .await;
                }
            };
            self.command_started(payload.event);
            let result = tokio::time::timeout_at(
                event_deadline,
                run_hook_command(
                    &hook,
                    &workspace_root,
                    input,
                    hook_authorization.clone(),
                    cancel.clone(),
                ),
            )
            .await;
            let output = match result {
                Ok(Ok(output)) => output,
                Ok(Err(failure)) => {
                    had_failure = true;
                    event_failure_code = Some(failure.code());
                    self.record_failure(payload.event, failure.code());
                    if payload.event == LifecycleHookEvent::PreToolUse {
                        self.finish_event(
                            payload.event,
                            LifecycleHookOutcome::FailedOpen,
                            Some(failure.code()),
                        );
                        return LifecycleHookDecision::Deny {
                            reason: failure.public_reason().to_owned(),
                        };
                    }
                    tracing::warn!(event = ?payload.event, failure = failure.code(), "lifecycle hook was skipped");
                    continue;
                }
                Err(_) => {
                    had_failure = true;
                    event_failure_code = Some("event_time_limit");
                    self.record_failure(payload.event, "event_time_limit");
                    if payload.event == LifecycleHookEvent::PreToolUse {
                        self.finish_event(
                            payload.event,
                            LifecycleHookOutcome::FailedOpen,
                            Some("event_time_limit"),
                        );
                        return LifecycleHookDecision::Deny {
                            reason: "PreToolUse hook exceeded its time limit".into(),
                        };
                    }
                    tracing::warn!(event = ?payload.event, "lifecycle hook event exceeded its time limit");
                    break;
                }
            };

            if payload.event == LifecycleHookEvent::PreToolUse {
                match parse_pre_tool_reply(&output.stdout) {
                    Ok(LifecycleHookDecision::Allow) => {}
                    Ok(denied @ LifecycleHookDecision::Deny { .. }) => {
                        self.finish_event(payload.event, LifecycleHookOutcome::Denied, None);
                        return denied;
                    }
                    Err(failure) => {
                        self.record_failure(payload.event, failure.code());
                        self.finish_event(
                            payload.event,
                            LifecycleHookOutcome::FailedOpen,
                            Some(failure.code()),
                        );
                        return LifecycleHookDecision::Deny {
                            reason: failure.public_reason().to_owned(),
                        };
                    }
                }
            }
        }
        if had_failure {
            self.finish_event(
                payload.event,
                LifecycleHookOutcome::FailedOpen,
                event_failure_code,
            );
        } else {
            self.finish_event(payload.event, LifecycleHookOutcome::Succeeded, None);
        }
        LifecycleHookDecision::Allow
    }

    pub(crate) fn diagnostics(&self) -> Vec<LifecycleHookEventDiagnostics> {
        let Ok(diagnostics) = self.diagnostics.lock() else {
            tracing::warn!("lifecycle hook diagnostics lock poisoned");
            return Vec::new();
        };
        let mut values = diagnostics.values().cloned().collect::<Vec<_>>();
        values.sort_by_key(|item| event_order(item.event));
        values
    }

    fn begin_event(&self, event: LifecycleHookEvent) {
        self.with_event_stats(event, |stats| {
            stats.event_runs = stats.event_runs.saturating_add(1);
            stats.last_outcome = None;
            stats.last_failure_code = None;
        });
    }

    fn command_started(&self, event: LifecycleHookEvent) {
        self.with_event_stats(event, |stats| {
            stats.command_runs = stats.command_runs.saturating_add(1);
        });
    }

    fn record_failure(&self, event: LifecycleHookEvent, code: &str) {
        self.with_event_stats(event, |stats| {
            stats.failures = stats.failures.saturating_add(1);
            stats.last_failure_code = Some(code.to_owned());
        });
    }

    fn finish_event(
        &self,
        event: LifecycleHookEvent,
        outcome: LifecycleHookOutcome,
        failure_code: Option<&str>,
    ) {
        self.with_event_stats(event, |stats| {
            stats.last_outcome = Some(outcome);
            if let Some(code) = failure_code {
                stats.last_failure_code = Some(code.to_owned());
            } else if outcome != LifecycleHookOutcome::FailedOpen {
                stats.last_failure_code = None;
            }
            if outcome == LifecycleHookOutcome::Denied {
                stats.denials = stats.denials.saturating_add(1);
            }
        });
    }

    fn with_event_stats(
        &self,
        event: LifecycleHookEvent,
        update: impl FnOnce(&mut LifecycleHookEventDiagnostics),
    ) {
        let Ok(mut diagnostics) = self.diagnostics.lock() else {
            tracing::warn!("lifecycle hook diagnostics lock poisoned");
            return;
        };
        let stats = diagnostics
            .entry(event)
            .or_insert_with(|| LifecycleHookEventDiagnostics {
                event,
                event_runs: 0,
                command_runs: 0,
                failures: 0,
                denials: 0,
                last_outcome: None,
                last_failure_code: None,
            });
        update(stats);
    }

    async fn failure_decision(
        &self,
        event: LifecycleHookEvent,
        failure: &'static str,
    ) -> LifecycleHookDecision {
        self.record_failure(event, failure);
        self.finish_event(event, LifecycleHookOutcome::FailedOpen, Some(failure));
        tracing::warn!(event = ?event, failure, "lifecycle hook event could not run");
        if event == LifecycleHookEvent::PreToolUse {
            LifecycleHookDecision::Deny {
                reason: "PreToolUse hook failed closed".into(),
            }
        } else {
            LifecycleHookDecision::Allow
        }
    }
}

fn event_order(event: LifecycleHookEvent) -> u8 {
    match event {
        LifecycleHookEvent::SessionStart => 0,
        LifecycleHookEvent::RunStart => 1,
        LifecycleHookEvent::PreToolUse => 2,
        LifecycleHookEvent::PostToolUse => 3,
        LifecycleHookEvent::PreCompact => 4,
        LifecycleHookEvent::PostCompact => 5,
        LifecycleHookEvent::RunEnd => 6,
    }
}

#[derive(Debug)]
enum HookFailure {
    Cancelled,
    PolicyChanged,
    TimeLimit,
    OutputLimit,
    Spawn,
    Io,
    NonZeroExit,
    InvalidReply,
}

impl HookFailure {
    fn code(&self) -> &'static str {
        match self {
            Self::Cancelled => "cancelled",
            Self::PolicyChanged => "policy_changed",
            Self::TimeLimit => "time_limit",
            Self::OutputLimit => "output_limit",
            Self::Spawn => "spawn_failed",
            Self::Io => "io_failed",
            Self::NonZeroExit => "nonzero_exit",
            Self::InvalidReply => "invalid_pre_tool_reply",
        }
    }

    fn public_reason(&self) -> &'static str {
        match self {
            Self::Cancelled => "PreToolUse hook was cancelled",
            Self::PolicyChanged => "PreToolUse hook stopped because the security policy changed",
            Self::TimeLimit => "PreToolUse hook exceeded its time limit",
            Self::OutputLimit => "PreToolUse hook exceeded its output limit",
            Self::Spawn | Self::Io => "PreToolUse hook could not be started or completed",
            Self::NonZeroExit | Self::InvalidReply => {
                "PreToolUse hook rejected or returned an invalid response"
            }
        }
    }
}

struct HookProcessOutput {
    stdout: Vec<u8>,
}

async fn run_hook_command(
    hook: &HookCommand,
    cwd: &Path,
    input: Vec<u8>,
    authorization: LifecycleHookProcessStartAuthorization,
    cancel: CancellationToken,
) -> Result<HookProcessOutput, HookFailure> {
    let mut command = tokio::process::Command::new(&hook.command);
    command
        .args(&hook.args)
        .current_dir(cwd)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .env_clear()
        .envs(
            haven_platform::process_environment::filter_runtime_baseline_environment(
                std::env::vars_os(),
            ),
        );

    #[cfg(windows)]
    let mut runtime_read_roots = crate::process_sandbox::runtime_dependency_roots(command.as_std());
    #[cfg(windows)]
    {
        let configured_program = PathBuf::from(&hook.command);
        if configured_program.is_absolute()
            && let Some(parent) = configured_program.parent()
            && haven_platform::filesystem::DirectoryCapability::open(parent).is_ok()
        {
            runtime_read_roots.push(parent.to_path_buf());
        }
    }
    #[cfg(not(windows))]
    let runtime_read_roots = Vec::new();
    let request = LifecycleHookProcessStartRequest {
        working_directory: cwd.to_path_buf(),
        runtime_read_roots,
    };
    let permit = tokio::select! {
        biased;
        _ = cancel.cancelled() => return Err(HookFailure::Cancelled),
        permit = authorization.acquire(request) => permit.map_err(|_| HookFailure::PolicyChanged)?,
    };

    if let Some(policy) = permit.sandboxed_process_policy() {
        #[cfg(windows)]
        {
            command.env("TEMP", cwd).env("TMP", cwd).env("TMPDIR", cwd);
            let request = haven_platform::sandboxed_process::SandboxedProcessRequest::from_command(
                command.as_std(),
                policy,
                false,
            )
            .map_err(|_| HookFailure::Spawn)?;
            let mut process = haven_platform::sandboxed_process::SandboxedProcess::spawn(
                request,
                policy.isolation_mode,
            )
            .await
            .map_err(|_| HookFailure::Spawn)?;
            drop(permit);

            let stdin = process.take_stdin().ok_or(HookFailure::Io)?;
            let stdout = process.take_stdout().ok_or(HookFailure::Io)?;
            let stderr = process.take_stderr().ok_or(HookFailure::Io)?;
            let process_deadline = tokio::time::Instant::now() + MAX_HOOK_PROCESS_DURATION;
            let result = tokio::select! {
                biased;
                _ = cancel.cancelled() => Err(HookFailure::Cancelled),
                _ = authorization.wait_for_revocation() => Err(HookFailure::PolicyChanged),
                _ = tokio::time::sleep_until(process_deadline) => Err(HookFailure::TimeLimit),
                result = async {
                    let write_input = async move {
                        let mut stdin = stdin;
                        stdin.write_all(&input).await?;
                        stdin.shutdown().await
                    };
                    let read_stdout = read_bounded(stdout, MAX_HOOK_OUTPUT_BYTES);
                    let read_stderr = read_bounded(stderr, MAX_HOOK_OUTPUT_BYTES);
                    let wait = async {
                        process
                            .wait()
                            .await
                            .map(|exit| exit.exit_code == 0)
                    };
                    let (_, stdout, _stderr, success) =
                        tokio::try_join!(write_input, read_stdout, read_stderr, wait)?;
                    Ok::<_, std::io::Error>((stdout, success))
                } => match result {
                    Ok((stdout, true)) => Ok(HookProcessOutput { stdout }),
                    Ok((_stdout, false)) => Err(HookFailure::NonZeroExit),
                    Err(error) if error.kind() == std::io::ErrorKind::InvalidData => Err(HookFailure::OutputLimit),
                    Err(_) => Err(HookFailure::Io),
                }
            };
            if result.is_err() {
                let _ = process.kill().await;
                let _ = tokio::time::timeout(PROCESS_CLEANUP_TIMEOUT, process.wait()).await;
            }
            return result;
        }
        #[cfg(not(windows))]
        {
            let _ = (policy, command);
            return Err(HookFailure::Spawn);
        }
    }
    let containment = ProcessContainment::new().map_err(|_| HookFailure::Spawn)?;
    containment.prepare_command(command.as_std_mut(), 0);

    let child_spawn_result = {
        let _spawn_guard = process_spawn_lock();
        command.kill_on_drop(true).spawn()
    };
    let mut child = child_spawn_result.map_err(|_| HookFailure::Spawn)?;
    let pid = child.id().ok_or(HookFailure::Spawn)?;
    #[cfg(windows)]
    let attach_result = child
        .raw_handle()
        .ok_or_else(|| std::io::Error::other("hook child process handle is unavailable"))
        .and_then(|handle| containment.attach_and_resume(pid, handle));
    #[cfg(not(windows))]
    let attach_result = containment.attach_and_resume(pid, ());
    if attach_result.is_err() {
        let _ = child.kill().await;
        let _ = child.wait().await;
        return Err(HookFailure::Spawn);
    }
    drop(permit);

    let stdin = child.stdin.take().ok_or(HookFailure::Io)?;
    let stdout = child.stdout.take().ok_or(HookFailure::Io)?;
    let stderr = child.stderr.take().ok_or(HookFailure::Io)?;
    let process_deadline = tokio::time::Instant::now() + MAX_HOOK_PROCESS_DURATION;
    let result = tokio::select! {
        biased;
        _ = cancel.cancelled() => Err(HookFailure::Cancelled),
        _ = authorization.wait_for_revocation() => Err(HookFailure::PolicyChanged),
        _ = tokio::time::sleep_until(process_deadline) => Err(HookFailure::TimeLimit),
        result = async {
            let write_input = async move {
                let mut stdin = stdin;
                stdin.write_all(&input).await?;
                stdin.shutdown().await
            };
            let read_stdout = read_bounded(stdout, MAX_HOOK_OUTPUT_BYTES);
            let read_stderr = read_bounded(stderr, MAX_HOOK_OUTPUT_BYTES);
            let wait = child.wait();
            let (_, stdout, _stderr, status) = tokio::try_join!(write_input, read_stdout, read_stderr, wait)?;
            Ok::<_, std::io::Error>((stdout, status))
        } => match result {
            Ok((stdout, status)) if status.success() => Ok(HookProcessOutput { stdout }),
            Ok((_stdout, _status)) => Err(HookFailure::NonZeroExit),
            Err(error) if error.kind() == std::io::ErrorKind::InvalidData => Err(HookFailure::OutputLimit),
            Err(_) => Err(HookFailure::Io),
        }
    };
    if result.is_err() {
        let _ = containment.terminate_and_wait(PROCESS_CLEANUP_TIMEOUT);
        let _ = child.kill().await;
        let _ = child.wait().await;
    }
    result
}

async fn read_bounded<R>(mut reader: R, limit: usize) -> std::io::Result<Vec<u8>>
where
    R: AsyncRead + Unpin,
{
    let mut output = Vec::with_capacity(limit.min(8192));
    let mut buffer = [0_u8; 8192];
    loop {
        let count = reader.read(&mut buffer).await?;
        if count == 0 {
            return Ok(output);
        }
        if output.len().saturating_add(count) > limit {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "lifecycle hook output limit exceeded",
            ));
        }
        output.extend_from_slice(&buffer[..count]);
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PreToolReply {
    decision: PreToolReplyDecision,
    #[serde(default)]
    reason: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum PreToolReplyDecision {
    Allow,
    Deny,
}

fn parse_pre_tool_reply(output: &[u8]) -> Result<LifecycleHookDecision, HookFailure> {
    if output.iter().all(u8::is_ascii_whitespace) {
        return Ok(LifecycleHookDecision::Allow);
    }
    let reply: PreToolReply =
        serde_json::from_slice(output).map_err(|_| HookFailure::InvalidReply)?;
    match reply.decision {
        PreToolReplyDecision::Allow => Ok(LifecycleHookDecision::Allow),
        PreToolReplyDecision::Deny => Ok(LifecycleHookDecision::Deny {
            reason: reply
                .reason
                .map(|reason| clip_reason(&reason))
                .filter(|reason| !reason.is_empty())
                .unwrap_or_else(|| "Denied by PreToolUse hook".into()),
        }),
    }
}

fn clip_reason(reason: &str) -> String {
    reason
        .chars()
        .filter(|ch| !ch.is_control())
        .take(512)
        .collect()
}

fn matcher_applies(hook: &HookCommand, tool_name: Option<&str>) -> bool {
    match hook.matcher.as_deref() {
        None | Some("*") => true,
        Some(matcher) => tool_name.is_some_and(|name| wildcard_matches(matcher, name)),
    }
}

/// Match `*` over Unicode scalar values without compiling regexes or adding a
/// dependency. Matcher length is capped by config validation.
fn wildcard_matches(pattern: &str, value: &str) -> bool {
    let pattern = pattern.chars().collect::<Vec<_>>();
    let value = value.chars().collect::<Vec<_>>();
    let (mut pattern_index, mut value_index) = (0, 0);
    let mut star_index = None;
    let mut retry_value_index = 0;

    while value_index < value.len() {
        if pattern_index < pattern.len()
            && pattern[pattern_index] != '*'
            && pattern[pattern_index] == value[value_index]
        {
            pattern_index += 1;
            value_index += 1;
        } else if pattern_index < pattern.len() && pattern[pattern_index] == '*' {
            star_index = Some(pattern_index);
            pattern_index += 1;
            retry_value_index = value_index;
        } else if let Some(star) = star_index {
            retry_value_index += 1;
            value_index = retry_value_index;
            pattern_index = star + 1;
        } else {
            return false;
        }
    }

    pattern[pattern_index..].iter().all(|ch| *ch == '*')
}

fn is_trusted_workspace(root: &Path, trusted: &[PathBuf]) -> bool {
    let Ok(canonical_root) = std::fs::canonicalize(root) else {
        return false;
    };
    trusted.iter().any(|trusted_root| {
        std::fs::canonicalize(trusted_root)
            .ok()
            .is_some_and(|candidate| same_path(&candidate, &canonical_root))
    })
}

fn same_path(left: &Path, right: &Path) -> bool {
    #[cfg(windows)]
    {
        left.to_string_lossy()
            .eq_ignore_ascii_case(&right.to_string_lossy())
    }
    #[cfg(not(windows))]
    {
        left == right
    }
}

fn read_project_hooks(
    directory: Option<&DirectoryCapability>,
) -> anyhow::Result<Option<LifecycleHooksConfig>> {
    let Some(directory) = directory else {
        anyhow::bail!("execution workspace directory capability is unavailable");
    };
    let mut file = match directory.open_read(Path::new(PROJECT_HOOKS_PATH)) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let metadata = file.metadata()?;
    anyhow::ensure!(
        metadata.is_file(),
        "project hook config is not a regular file"
    );
    anyhow::ensure!(
        metadata.len() <= MAX_PROJECT_CONFIG_BYTES as u64,
        "project hook config exceeds its size limit"
    );
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.by_ref()
        .take((MAX_PROJECT_CONFIG_BYTES + 1) as u64)
        .read_to_end(&mut bytes)?;
    anyhow::ensure!(
        bytes.len() <= MAX_PROJECT_CONFIG_BYTES,
        "project hook config exceeds its size limit"
    );
    let text = std::str::from_utf8(&bytes)?;
    Ok(Some(parse_project_hooks(text)?))
}

fn bounded_payload(payload: &mut LifecycleHookPayload) -> anyhow::Result<Vec<u8>> {
    let mut bytes = serde_json::to_vec(payload)?;
    if bytes.len() <= MAX_HOOK_PAYLOAD_BYTES {
        return Ok(bytes);
    }
    payload.payload_truncated = true;
    payload.tool_output = None;
    bytes = serde_json::to_vec(payload)?;
    if bytes.len() <= MAX_HOOK_PAYLOAD_BYTES {
        return Ok(bytes);
    }
    payload.tool_input = None;
    bytes = serde_json::to_vec(payload)?;
    anyhow::ensure!(
        bytes.len() <= MAX_HOOK_PAYLOAD_BYTES,
        "lifecycle hook metadata exceeds the input limit"
    );
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use haven_common::config::{ConfigLoader, ConfigService, InMemoryCredentialStore};

    #[test]
    fn matcher_supports_exact_names_global_and_partial_wildcards() {
        let exact = HookCommand {
            enabled: true,
            command: "hook.exe".into(),
            args: Vec::new(),
            matcher: Some("shell".into()),
        };
        assert!(matcher_applies(&exact, Some("shell")));
        assert!(!matcher_applies(&exact, Some("files.read")));
        assert!(!matcher_applies(&exact, None));

        let all = HookCommand {
            matcher: Some("*".into()),
            ..exact
        };
        assert!(matcher_applies(&all, None));

        let namespace = HookCommand {
            matcher: Some("mcp_*.search".into()),
            ..all.clone()
        };
        assert!(matcher_applies(&namespace, Some("mcp_drive.search")));
        assert!(!matcher_applies(&namespace, Some("mcp_drive.read")));

        let middle = HookCommand {
            matcher: Some("files.*.read".into()),
            ..all
        };
        assert!(matcher_applies(&middle, Some("files.workspace.read")));
        assert!(!matcher_applies(&middle, Some("files.read")));
    }

    #[test]
    fn pre_tool_reply_can_only_allow_or_deny_and_clips_reason() {
        assert_eq!(
            parse_pre_tool_reply(br#"{"decision":"allow"}"#).unwrap(),
            LifecycleHookDecision::Allow
        );
        assert_eq!(
            parse_pre_tool_reply(br#"{"decision":"deny","reason":"blocked"}"#).unwrap(),
            LifecycleHookDecision::Deny {
                reason: "blocked".into()
            }
        );
        assert!(parse_pre_tool_reply(br#"{"decision":"prompt"}"#).is_err());
        assert!(parse_pre_tool_reply(br#"{"decision":"allow","extra":true}"#).is_err());
    }

    #[test]
    fn payload_drops_large_tool_result_then_input() {
        let mut payload = LifecycleHookPayload::new(LifecycleHookEvent::PostToolUse);
        payload.tool_input = Some(serde_json::json!({"input": "x".repeat(MAX_HOOK_PAYLOAD_BYTES)}));
        payload.tool_output =
            Some(serde_json::json!({"output": "y".repeat(MAX_HOOK_PAYLOAD_BYTES)}));

        let bytes = bounded_payload(&mut payload).unwrap();

        assert!(bytes.len() <= MAX_HOOK_PAYLOAD_BYTES);
        assert!(payload.payload_truncated);
        assert!(payload.tool_output.is_none());
        assert!(payload.tool_input.is_none());
    }

    #[tokio::test]
    async fn diagnostics_are_content_free_and_count_unconfigured_events() {
        let hooks = LifecycleHookService::default();
        let decision = hooks
            .run(
                &Arc::new(AuthorizationEngine::new()),
                LifecycleHookPayload::new(LifecycleHookEvent::RunStart),
                CancellationToken::new(),
            )
            .await;

        assert_eq!(decision, LifecycleHookDecision::Allow);
        let diagnostics = hooks.diagnostics();
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].event_runs, 1);
        assert_eq!(diagnostics[0].command_runs, 0);
        assert_eq!(
            diagnostics[0].last_outcome,
            Some(LifecycleHookOutcome::NoHooks)
        );
        assert!(diagnostics[0].last_failure_code.is_none());
    }

    #[test]
    fn project_hook_trust_matches_only_the_canonical_workspace_root() {
        let workspace = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();

        assert!(is_trusted_workspace(
            workspace.path(),
            &[workspace.path().to_path_buf()]
        ));
        assert!(!is_trusted_workspace(
            workspace.path(),
            &[other.path().to_path_buf()]
        ));
    }

    #[tokio::test]
    async fn pre_tool_hook_fails_closed_when_the_active_sandbox_cannot_run_it() {
        let directory = tempfile::tempdir().unwrap();
        let config_path = directory.path().join("config.toml");
        std::fs::write(
            &config_path,
            "[[hooks.pre_tool_use]]\ncommand = \"must-not-run.exe\"\nmatcher = \"shell\"\n",
        )
        .unwrap();
        let loader = ConfigLoader::load_from(&config_path).unwrap();
        let config_service = Arc::new(
            ConfigService::new_with_credential_store(
                loader,
                Arc::new(InMemoryCredentialStore::default()),
            )
            .unwrap(),
        );
        let hooks = LifecycleHookService::default();
        hooks.bind_config_service(Some(config_service)).unwrap();
        let authorization = Arc::new(AuthorizationEngine::new());
        let mut payload = LifecycleHookPayload::new(LifecycleHookEvent::PreToolUse);
        payload.tool_name = Some("shell".into());

        let decision = hooks
            .run(&authorization, payload, CancellationToken::new())
            .await;

        assert!(matches!(decision, LifecycleHookDecision::Deny { .. }));
    }

    #[tokio::test]
    async fn disabled_user_and_project_hooks_never_reach_process_authorization() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::create_dir(directory.path().join(".haven")).unwrap();
        let disabled = "[[hooks.pre_tool_use]]\ncommand = 'must-not-run.exe'\nenabled = false\n";
        std::fs::write(directory.path().join(".haven/hooks.toml"), disabled).unwrap();
        let config_path = directory.path().join("config.toml");
        let trusted_root = directory.path().to_string_lossy().replace('\\', "/");
        std::fs::write(
            &config_path,
            format!("trusted_hook_workspaces = ['{trusted_root}']\n{disabled}"),
        )
        .unwrap();
        let config_service = Arc::new(
            ConfigService::new_with_credential_store(
                ConfigLoader::load_from(&config_path).unwrap(),
                Arc::new(InMemoryCredentialStore::default()),
            )
            .unwrap(),
        );
        let hooks = LifecycleHookService::default();
        hooks
            .bind_config_service(Some(config_service.clone()))
            .unwrap();
        let authorization = Arc::new(AuthorizationEngine::new());
        authorization
            .switch_execution_workspace(directory.path())
            .await
            .unwrap();
        authorization
            .set_boundaries(
                haven_common::types::SandboxMode::ReadOnly,
                Vec::new(),
                haven_common::types::NetworkPolicy::Open,
            )
            .await;
        // ReadOnly cannot launch Hook commands. A disabled veto must not turn
        // this policy rejection into a denied tool call.
        let mut payload = LifecycleHookPayload::new(LifecycleHookEvent::PreToolUse);
        payload.tool_name = Some("shell".into());
        assert_eq!(
            hooks
                .run(&authorization, payload.clone(), CancellationToken::new())
                .await,
            LifecycleHookDecision::Allow
        );
        assert_eq!(hooks.diagnostics()[0].command_runs, 0);
        assert_eq!(
            hooks.diagnostics()[0].last_outcome,
            Some(LifecycleHookOutcome::NoHooks)
        );

        // Re-enabling a veto through the authoritative config takes effect on
        // the next event and restores fail-closed policy enforcement.
        let mut enabled = config_service.snapshot().unwrap().config.hooks.clone();
        enabled.pre_tool_use[0].enabled = true;
        config_service
            .apply_patch(haven_common::config::ConfigPatch::LifecycleHooks(enabled))
            .unwrap();
        assert!(matches!(
            hooks
                .run(&authorization, payload, CancellationToken::new())
                .await,
            LifecycleHookDecision::Deny { .. }
        ));
        assert_eq!(
            hooks.diagnostics()[0].last_failure_code.as_deref(),
            Some("security_policy_denied")
        );
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn configured_command_runs_directly_inside_process_containment() {
        use haven_common::types::{NetworkPolicy, SandboxMode};

        let directory = tempfile::tempdir().unwrap();
        let authorization = Arc::new(AuthorizationEngine::new());
        authorization
            .set_boundaries(SandboxMode::FullAccess, Vec::new(), NetworkPolicy::Open)
            .await;
        let process_authorization = authorization
            .capture_lifecycle_hook_process_start_authorization()
            .await
            .unwrap();
        let hook = HookCommand {
            enabled: true,
            command: std::env::var("COMSPEC").unwrap(),
            args: vec!["/D".into(), "/C".into(), "echo lifecycle-hook-ok".into()],
            matcher: None,
        };

        let output = run_hook_command(
            &hook,
            directory.path(),
            br#"{"event":"session_start"}"#.to_vec(),
            process_authorization,
            CancellationToken::new(),
        )
        .await
        .unwrap();

        assert!(String::from_utf8_lossy(&output.stdout).contains("lifecycle-hook-ok"));
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn workspace_write_hook_runs_in_mxc_with_workspace_write_access() {
        use haven_common::types::{NetworkPolicy, SandboxMode};

        let directory = tempfile::tempdir().unwrap();
        let workspace = directory.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let authorization = Arc::new(AuthorizationEngine::new());
        authorization
            .switch_execution_workspace(&workspace)
            .await
            .unwrap();
        authorization
            .set_boundaries(SandboxMode::WorkspaceWrite, Vec::new(), NetworkPolicy::Open)
            .await;
        let process_authorization = authorization
            .capture_lifecycle_hook_process_start_authorization()
            .await
            .unwrap();
        let hook = HookCommand {
            enabled: true,
            command: std::env::var("COMSPEC").unwrap(),
            args: vec![
                "/D".into(),
                "/C".into(),
                "echo lifecycle-hook-mxc>hook-output.txt".into(),
            ],
            matcher: None,
        };

        run_hook_command(
            &hook,
            &workspace,
            br#"{"event":"session_start"}"#.to_vec(),
            process_authorization,
            CancellationToken::new(),
        )
        .await
        .unwrap();

        let output = std::fs::read_to_string(workspace.join("hook-output.txt")).unwrap();
        assert!(output.contains("lifecycle-hook-mxc"));
    }
}
