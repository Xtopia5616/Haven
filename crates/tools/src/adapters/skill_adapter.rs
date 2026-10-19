use async_trait::async_trait;
use haven_common::tools::{ToolCatalogGroup, ToolSource};
use haven_common::types::RiskLevel;
use haven_skills::Skill;
use serde_json::Value;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

use super::{
    sanitize_external_description, skill_execution_policy_for_sandbox, skill_input_schema,
    skill_operation_spec,
};
use crate::operation_view::OperationSpec;
use crate::skill_runner::SkillRunner;
use crate::{StructuredToolError, Tool, ToolAuthorizationContract, ToolErrorMetadata, ToolResult};
// ---------------------------------------------------------------------------
// SkillToolAdapter — wraps a Skill as a dyn Tool
// ---------------------------------------------------------------------------

pub struct SkillToolAdapter {
    skill: Arc<Skill>,
    runner: SkillRunner,
    spec: OperationSpec,
    authorization: Option<Arc<crate::security::AuthorizationEngine>>,
    #[cfg(windows)]
    windows_container_execution: Option<
        Arc<dyn haven_platform::windows_container::execution::WindowsContainerExecutionPort>,
    >,
    #[cfg(windows)]
    sandbox_output_reviews: Option<crate::SandboxOutputReviewOwner>,
    #[cfg(all(debug_assertions, any(test, feature = "test-support")))]
    panic_on_execute: bool,
}

impl SkillToolAdapter {
    pub fn new(skill: Arc<Skill>, runner: SkillRunner) -> Self {
        let name = skill_tool_name(skill.name());
        let description = sanitize_external_description(skill.description());
        let spec = skill_operation_spec(&name, &description, skill_input_schema());
        Self {
            skill,
            runner,
            spec,
            authorization: None,
            #[cfg(windows)]
            windows_container_execution: None,
            #[cfg(windows)]
            sandbox_output_reviews: None,
            #[cfg(all(debug_assertions, any(test, feature = "test-support")))]
            panic_on_execute: false,
        }
    }

    pub(crate) fn with_authorization_engine(
        mut self,
        authorization: Arc<crate::security::AuthorizationEngine>,
    ) -> Self {
        self.authorization = Some(authorization);
        self
    }

    #[cfg(windows)]
    pub(crate) fn with_windows_container_runtime(
        mut self,
        execution: Arc<
            dyn haven_platform::windows_container::execution::WindowsContainerExecutionPort,
        >,
        reviews: crate::SandboxOutputReviewOwner,
    ) -> Self {
        self.windows_container_execution = Some(execution);
        self.sandbox_output_reviews = Some(reviews);
        self
    }

    /// Construct a real Skill adapter that panics when executed. This is only
    /// available in debug builds for the agent's extension-boundary tests.
    #[cfg(all(debug_assertions, any(test, feature = "test-support")))]
    #[doc(hidden)]
    pub fn new_panicking_for_test(skill: Arc<Skill>, runner: SkillRunner) -> Self {
        let mut adapter = Self::new(skill, runner);
        adapter.panic_on_execute = true;
        adapter
    }

    /// Canonical qualified tool name (`skill::<name>`) after LLM name
    /// sanitization. The same deterministic name is used for catalog
    /// registration and execution lookup.
    fn qualified_name(&self) -> String {
        skill_tool_name(self.skill.name())
    }

    async fn execute_with_authorization(
        &self,
        input: Value,
        cancel: CancellationToken,
        execution_authorization: crate::security::ExecutionPolicyAuthorization,
    ) -> anyhow::Result<ToolResult> {
        #[cfg(all(debug_assertions, any(test, feature = "test-support")))]
        if self.panic_on_execute {
            panic!("simulated Skill adapter panic");
        }
        let params = input.get("params").ok_or_else(|| {
            anyhow::Error::new(StructuredToolError::new(
                "skill parameters are required",
                ToolErrorMetadata::validation(),
            ))
        })?;
        self.runner
            .execute_with_authorization(&self.skill, params, cancel, execution_authorization)
            .await
            .map_err(|error| {
                anyhow::Error::new(StructuredToolError::new(
                    error.to_string(),
                    ToolErrorMetadata::other(),
                ))
            })
    }

    #[cfg(windows)]
    async fn execute_workspace_write(
        &self,
        params: &Value,
        cancel: CancellationToken,
        context: &crate::ToolExecutionContext,
        authorization: crate::security::ExecutionPolicyAuthorization,
    ) -> anyhow::Result<ToolResult> {
        use crate::SandboxOutputReviewPort;
        use haven_platform::filesystem::{
            DirectoryCapability, DirectoryTreeCopyLimits, DirectoryTreeDiffLimits,
        };
        use haven_platform::windows_container::execution::{
            WORKSPACE_WRITE_STORAGE_QUOTA_BYTES, WindowsContainerExecutionLimits,
            WindowsContainerOutputStream, WindowsContainerSkillBundleInput,
            WindowsContainerSkillExecutionContract, WindowsContainerSkillExecutionRequest,
            WindowsContainerWorkspaceInput,
        };
        use std::io;
        use std::time::Duration;

        const REVIEW_CHARGE_BYTES: u64 = 384 * 1024 * 1024;
        const WORKSPACE_BYTES: u64 = 128 * 1024 * 1024;
        const WORKSPACE_FILE_BYTES: u64 = 64 * 1024 * 1024;
        const MAX_CAPTURE_BYTES: usize = 8 * 1024 * 1024;

        let execution = self.windows_container_execution.as_ref().ok_or_else(|| {
            crate::security::execution_policy_denied(
                "workspace-write Skill requires the Windows container backend".into(),
            )
        })?;
        let reviews = self.sandbox_output_reviews.as_ref().ok_or_else(|| {
            crate::security::execution_policy_denied(
                "workspace-write Skill output review is unavailable".into(),
            )
        })?;
        let session_id = context.call_session_id.as_deref().ok_or_else(|| {
            crate::security::execution_policy_denied(
                "workspace-write Skill requires a canonical session id".into(),
            )
        })?;
        let source_step_id = context.call_step_id.as_deref().ok_or_else(|| {
            crate::security::execution_policy_denied(
                "workspace-write Skill requires a canonical source step id".into(),
            )
        })?;
        let directory = context
            .execution_workspace
            .as_ref()
            .and_then(crate::execution_workspace::ExecutionWorkspace::directory)
            .ok_or_else(|| {
                crate::security::execution_policy_denied(
                    "workspace-write Skill has no authorized execution workspace".into(),
                )
            })?;
        let (policy_revision, workspace_generation) = authorization.policy_stamp();
        let (entry_script, entry_file) = skill_entry_relative_path(&self.skill)?;
        let parameters_json = serde_json::to_vec(params)?;
        let contract = WindowsContainerSkillExecutionContract::new(entry_script, parameters_json)?;
        let skill_root = self.skill.root().canonicalize()?;
        let skill_bundle =
            WindowsContainerSkillBundleInput::new(DirectoryCapability::open(&skill_root)?);
        let copy_limits =
            DirectoryTreeCopyLimits::new(10_000, 32, WORKSPACE_FILE_BYTES, WORKSPACE_BYTES)?;
        let diff_limits = DirectoryTreeDiffLimits::new(
            copy_limits,
            4_096,
            WORKSPACE_FILE_BYTES,
            WORKSPACE_BYTES,
        )?;
        let output_bytes = MAX_CAPTURE_BYTES;
        let limits = WindowsContainerExecutionLimits::new(
            WORKSPACE_WRITE_STORAGE_QUOTA_BYTES,
            Duration::from_secs(self.runner.timeout_secs()),
            output_bytes,
            output_bytes,
            copy_limits,
            diff_limits,
        )?;
        let request = WindowsContainerSkillExecutionRequest::new(
            contract,
            workspace_generation,
            policy_revision,
            limits,
        )?;
        let reservation = reviews
            .reserve(session_id, source_step_id, REVIEW_CHARGE_BYTES)
            .await
            .map_err(anyhow::Error::msg)?;
        if cancel.is_cancelled() {
            return Ok(ToolResult::cancelled("Skill execution cancelled"));
        }

        // Keep the validated entry handle open until the bundle has been
        // copied by the provider. On Windows this denies write/delete sharing
        // while preventing a path replacement between validation and copy.
        let workspace = WindowsContainerWorkspaceInput::new(directory);
        let port = Arc::clone(execution);
        let execution_cancel = cancel.child_token();
        let worker_cancel = execution_cancel.clone();
        let runtime = tokio::runtime::Handle::current();
        let start_authorization = authorization.clone();
        let spawn_permit = Arc::new(std::sync::Mutex::new(None));
        let permit_for_start = Arc::clone(&spawn_permit);
        let mut before_skill_start = move || {
            if permit_for_start
                .lock()
                .map_err(|_| io::Error::other("Skill start permit is unavailable"))?
                .is_some()
            {
                return Err(io::Error::other("Skill start permit was already acquired"));
            }
            let permit = runtime
                .block_on(start_authorization.acquire())
                .map_err(|reason| io::Error::new(io::ErrorKind::PermissionDenied, reason))?;
            if permit.sandbox_mode() != haven_common::types::SandboxMode::WorkspaceWrite
                || start_authorization.execution_boundary()
                    != crate::ExecutionBoundary::WorkspaceIsolated
            {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "Skill execution boundary changed before guest start",
                ));
            }
            let mut slot = permit_for_start
                .lock()
                .map_err(|_| io::Error::other("Skill start permit is unavailable"))?;
            if slot.is_some() {
                return Err(io::Error::other("Skill start permit was already acquired"));
            }
            *slot = Some(permit);
            Ok(())
        };
        let permit_for_started = Arc::clone(&spawn_permit);
        let mut skill_started = move || {
            if let Ok(mut permit) = permit_for_started.lock() {
                drop(permit.take());
            }
        };
        let mut is_cancelled = move || worker_cancel.is_cancelled();
        let mut task = tokio::task::spawn_blocking(move || {
            let _entry_file = entry_file;
            let mut on_output = move |_stream: WindowsContainerOutputStream, _bytes: &[u8]| {};
            let mut callbacks = haven_platform::windows_container::execution::WindowsContainerExecutionCallbacks::new(
                &mut before_skill_start,
                &mut skill_started,
                &mut is_cancelled,
                &mut on_output,
            );
            port.execute_skill(&request, &workspace, &skill_bundle, &mut callbacks)
        });

        let revoke_authorization = authorization.clone();
        let policy_revoked = revoke_authorization.wait_for_policy_revocation();
        tokio::pin!(policy_revoked);
        let result = tokio::select! {
            result = &mut task => result
                .map_err(|_| anyhow::anyhow!("container Skill worker failed"))?,
            _ = cancel.cancelled() => {
                execution_cancel.cancel();
                let _ = task.await;
                return Ok(ToolResult::cancelled("Skill execution cancelled"));
            }
            _ = &mut policy_revoked => {
                execution_cancel.cancel();
                let _ = task.await;
                return Ok(ToolResult::cancelled("execution policy changed; Skill execution cancelled"));
            }
        }
        .map_err(|error| {
            if error.kind() == io::ErrorKind::PermissionDenied {
                crate::security::execution_policy_denied(error.to_string())
            } else {
                anyhow::anyhow!(haven_common::error::sanitize_error_text(&error.to_string()))
            }
        })?;
        let stdout_bytes = result.stdout().to_vec();
        let stderr_bytes = result.stderr().to_vec();
        let exit_code = result.exit_code();
        let stdout_truncated = result.stdout_truncated();
        let stderr_truncated = result.stderr_truncated();
        reservation
            .retain(result.into_output_review())
            .map_err(anyhow::Error::msg)?;

        let stdout = haven_common::encoding::decode_lossy(&stdout_bytes);
        let stderr = haven_common::error::sanitize_error_text(
            &haven_common::encoding::decode_lossy(&stderr_bytes),
        );
        let max_lines = self.runner.max_output_lines();
        let stdout_lines = stdout.lines().take(max_lines.saturating_add(1)).count();
        let stderr_lines = stderr.lines().take(max_lines.saturating_add(1)).count();
        let stdout_preview = stdout
            .lines()
            .take(max_lines)
            .collect::<Vec<_>>()
            .join("\n");
        let stderr_preview = stderr
            .lines()
            .take(max_lines)
            .collect::<Vec<_>>()
            .join("\n");
        if stdout_truncated
            || stderr_truncated
            || stdout_lines > max_lines
            || stderr_lines > max_lines
        {
            return Ok(ToolResult::failed(
                serde_json::json!({
                    "stdout": stdout_preview,
                    "stderr": stderr_preview,
                    "truncated": true,
                }),
                format!("Skill '{}' output exceeded its limit", self.skill.name()),
            ));
        }
        if exit_code != 0 || !stderr_preview.trim().is_empty() {
            return Ok(ToolResult::failed(
                serde_json::json!({ "stdout": stdout_preview, "stderr": stderr_preview }),
                format!(
                    "Skill '{}' exited with code {}: {}",
                    self.skill.name(),
                    exit_code,
                    stderr_preview
                ),
            ));
        }
        let output = serde_json::from_str(&stdout_preview).map_err(|error| {
            let safe_error = haven_common::error::sanitize_error_text(&error.to_string());
            anyhow::anyhow!(
                "Skill '{}' returned invalid JSON: {safe_error}",
                self.skill.name()
            )
        })?;
        Ok(ToolResult::ok(output))
    }
}

#[cfg(windows)]
fn skill_entry_relative_path(skill: &Skill) -> anyhow::Result<(String, std::fs::File)> {
    let (entry, file) = skill
        .open_entry_script()
        .ok_or_else(|| anyhow::anyhow!("Skill '{}' has no valid entry script", skill.name()))?;
    let root = skill.root().canonicalize()?;
    let relative = entry
        .strip_prefix(root)
        .map_err(|_| anyhow::anyhow!("Skill entry script escaped its package root"))?;
    let path = relative
        .components()
        .map(|component| match component {
            std::path::Component::Normal(value) => Ok(value.to_string_lossy()),
            _ => Err(anyhow::anyhow!("Skill entry script path is invalid")),
        })
        .collect::<anyhow::Result<Vec<_>>>()?
        .join("/");
    Ok((path, file))
}

/// Return the canonical qualified tool name for an executable skill.
///
/// App authorization and Tools registration must use the same key so UI
/// preview permissions apply to the skill tool invocation.
pub fn skill_tool_name(skill_name: &str) -> String {
    crate::llm_tool_name(&format!("skill::{}", skill_name))
}

#[async_trait]
impl Tool for SkillToolAdapter {
    fn name(&self) -> String {
        self.qualified_name()
    }

    fn description(&self) -> String {
        sanitize_external_description(self.skill.description())
    }

    fn catalog_group(&self) -> ToolCatalogGroup {
        ToolCatalogGroup::Extensions
    }

    fn represented_source(&self) -> ToolSource {
        ToolSource::Skill
    }

    fn operation_spec(&self) -> Option<OperationSpec> {
        Some(self.spec.clone())
    }

    async fn authorization_contract(
        &self,
        input: &Value,
    ) -> Result<ToolAuthorizationContract, String> {
        let mut policy = self.spec.policy_for(input);
        if let Some(authorization) = &self.authorization {
            let sandbox_mode = authorization.sandbox_mode().await;
            policy = skill_execution_policy_for_sandbox(policy, sandbox_mode);
            if sandbox_mode == haven_common::types::SandboxMode::WorkspaceWrite {
                #[cfg(windows)]
                if self.windows_container_execution.is_none()
                    || self.sandbox_output_reviews.is_none()
                {
                    return Err(
                            "workspace-write Skill requires the Windows container and output review backends"
                                .into(),
                        );
                }
                #[cfg(not(windows))]
                return Err("workspace-write Skill requires the Windows container backend".into());
            }
        }
        Ok(ToolAuthorizationContract {
            policy,
            target_binding: None,
        })
    }

    fn risk_level(&self, input: &Value) -> RiskLevel {
        self.spec.policy_for(input).risk_level
    }

    fn input_schema(&self) -> Value {
        skill_input_schema()
    }

    fn default_timeout_secs(&self) -> u64 {
        self.runner.timeout_secs().saturating_add(5).max(30)
    }

    async fn execute(
        &self,
        _input: Value,
        _cancel: CancellationToken,
    ) -> anyhow::Result<ToolResult> {
        anyhow::bail!("Skill execution requires an authorization context")
    }

    async fn execute_with_authorization_context(
        &self,
        input: Value,
        cancel: CancellationToken,
        target_binding: Option<&str>,
        context: &crate::ToolExecutionContext,
    ) -> anyhow::Result<ToolResult> {
        if target_binding.is_some() {
            anyhow::bail!("skill tool cannot enforce an external authorization target");
        }
        let execution_authorization = context.clone_execution_authorization().ok_or_else(|| {
            crate::security::execution_policy_denied(
                "Skill execution requires an authorization proof".into(),
            )
        })?;
        let permit = match execution_authorization.acquire().await {
            Ok(permit) => permit,
            Err(_) => {
                return Ok(ToolResult::cancelled(
                    "Skill execution authorization changed before start",
                ));
            }
        };
        let sandbox_mode = permit.sandbox_mode();
        drop(permit);
        match sandbox_mode {
            haven_common::types::SandboxMode::FullAccess
                if execution_authorization.execution_boundary()
                    == crate::ExecutionBoundary::HostProcess =>
            {
                self.execute_with_authorization(input, cancel, execution_authorization)
                    .await
            }
            haven_common::types::SandboxMode::WorkspaceWrite
                if execution_authorization.execution_boundary()
                    == crate::ExecutionBoundary::WorkspaceIsolated =>
            {
                #[cfg(windows)]
                {
                    let params = input.get("params").ok_or_else(|| {
                        anyhow::Error::new(StructuredToolError::new(
                            "skill parameters are required",
                            ToolErrorMetadata::validation(),
                        ))
                    })?;
                    self.execute_workspace_write(params, cancel, context, execution_authorization)
                        .await
                }
                #[cfg(not(windows))]
                {
                    Err(crate::security::execution_policy_denied(
                        "workspace-write Skill requires the Windows container backend".into(),
                    ))
                }
            }
            haven_common::types::SandboxMode::ReadOnly => {
                Err(crate::security::execution_policy_denied(
                    "Skill execution is blocked by read-only sandbox mode".into(),
                ))
            }
            _ => Err(crate::security::execution_policy_denied(
                "Skill authorization does not match the live execution boundary".into(),
            )),
        }
    }
}
