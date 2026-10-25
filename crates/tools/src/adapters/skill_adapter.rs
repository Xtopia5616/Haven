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

/// Return the canonical qualified tool name for an executable skill.
pub fn skill_tool_name(skill_name: &str) -> String {
    crate::llm_tool_name(&format!("skill::{}", skill_name))
}

// ---------------------------------------------------------------------------
// SkillToolAdapter — wraps a Skill as a dyn Tool
// ---------------------------------------------------------------------------

pub struct SkillToolAdapter {
    skill: Arc<Skill>,
    runner: SkillRunner,
    spec: OperationSpec,
    authorization: Option<Arc<crate::security::AuthorizationEngine>>,
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
            if sandbox_mode == haven_common::types::SandboxMode::WorkspaceWrite && !cfg!(windows) {
                return Err("Workspace Write Skill processes require Windows MXC isolation".into());
            }
        }
        Ok(ToolAuthorizationContract {
            policy,
            target_binding: None,
            resources: Vec::new(),
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
                    == crate::ExecutionBoundary::SandboxedHost
                    && cfg!(windows) =>
            {
                self.execute_with_authorization(input, cancel, execution_authorization)
                    .await
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
