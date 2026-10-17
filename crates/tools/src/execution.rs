use super::*;
#[cfg(test)]
use crate::circuit::ToolCircuitRegistry;
use haven_common::retry::{
    BackoffPolicy, RecoveryDecision, RecoveryPolicy, RecoverySignal, RetryJitter,
};

/// Pure execution entry: admit the call, validate it, run the handler, and
/// classify the outcome.
///
/// `ToolsFacade` only forwards to this type. Interactive authorization is
/// assembled here and decided by the caller before `execute`, because a
/// missing confirmation receipt must fail closed without blocking inside the
/// tool future.
pub(crate) struct AuthorizedExecutor<'a> {
    tools: &'a ToolsFacade,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExecutionSurface {
    Model,
    CodeModeSubcall,
    WorkflowSubcall,
}

/// Proof-carrying execution request for the synthetic model-facing CodeMode
/// tool. Constructed by Agent after it binds the call to one session catalog.
pub struct AuthorizedCodeModeToolExecution {
    pub session_id: Option<String>,
    pub tool_name: String,
    pub input: Value,
    pub cancel: CancellationToken,
    pub step_id: Option<String>,
    pub proof: crate::AuthorizationExecutionProof,
    pub subcalls: Arc<dyn crate::CodeModeSubcallPort>,
}

/// Proof-carrying request for one CodeMode-only MCP child operation.
pub struct AuthorizedCodeModeSubcallExecution {
    pub session_id: Option<String>,
    pub tool_name: String,
    pub input: Value,
    pub cancel: CancellationToken,
    pub step_id: Option<String>,
    pub proof: crate::AuthorizationExecutionProof,
}

/// Proof-carrying request for one Legacy-only Workflow child operation.
pub struct AuthorizedWorkflowSubcallExecution {
    pub session_id: Option<String>,
    pub tool_name: String,
    pub input: Value,
    pub cancel: CancellationToken,
    pub step_id: Option<String>,
    pub proof: crate::AuthorizationExecutionProof,
}

/// Proof-carrying request for the synthetic model-facing Workflow tool.
pub struct AuthorizedWorkflowToolExecution {
    pub session_id: Option<String>,
    pub tool_name: String,
    pub input: Value,
    pub cancel: CancellationToken,
    pub step_id: Option<String>,
    pub proof: crate::AuthorizationExecutionProof,
    pub subcalls: Arc<dyn crate::WorkflowSubcallPort>,
}

struct ExecutionRequest {
    session_id: Option<String>,
    tool_name: String,
    input: Value,
    cancel: CancellationToken,
    step_id: Option<String>,
    authorization_proof: Option<crate::AuthorizationExecutionProof>,
    surface: ExecutionSurface,
    codemode_subcalls: Option<Arc<dyn crate::CodeModeSubcallPort>>,
    workflow_subcalls: Option<Arc<dyn crate::WorkflowSubcallPort>>,
}

impl ToolsFacade {
    pub(crate) fn executor(&self) -> AuthorizedExecutor<'_> {
        AuthorizedExecutor { tools: self }
    }

    #[cfg(any(test, feature = "test-support"))]
    pub async fn execute_tool(
        &self,
        session_id: Option<&str>,
        tool_name: &str,
        input: Value,
        cancel: CancellationToken,
    ) -> anyhow::Result<ToolResult> {
        self.executor()
            .execute_tool(session_id, tool_name, input, cancel)
            .await
    }

    #[cfg(any(test, feature = "test-support"))]
    pub async fn execute_tool_with_step(
        &self,
        session_id: Option<&str>,
        tool_name: &str,
        input: Value,
        cancel: CancellationToken,
        step_id: Option<&str>,
    ) -> anyhow::Result<ToolResult> {
        self.executor()
            .execute_tool_with_step(session_id, tool_name, input, cancel, step_id)
            .await
    }

    /// Execute one operation using a proof returned by the authorization owner.
    /// The proof is revalidated against the live operation contract at the
    /// start of every attempt, including retries.
    pub async fn execute_authorized_tool_with_step(
        &self,
        session_id: Option<&str>,
        tool_name: &str,
        input: Value,
        cancel: CancellationToken,
        step_id: Option<&str>,
        proof: crate::AuthorizationExecutionProof,
    ) -> anyhow::Result<ToolResult> {
        self.executor()
            .execute_authorized_tool_with_step(session_id, tool_name, input, cancel, step_id, proof)
            .await
    }

    pub async fn execute_authorized_codemode_tool_with_step(
        &self,
        execution: AuthorizedCodeModeToolExecution,
    ) -> anyhow::Result<ToolResult> {
        self.executor()
            .execute_authorized_codemode_tool_with_step(execution)
            .await
    }

    pub async fn execute_authorized_codemode_subcall_with_step(
        &self,
        execution: AuthorizedCodeModeSubcallExecution,
    ) -> anyhow::Result<ToolResult> {
        self.executor()
            .execute_authorized_codemode_subcall_with_step(execution)
            .await
    }

    pub async fn execute_authorized_workflow_subcall_with_step(
        &self,
        execution: AuthorizedWorkflowSubcallExecution,
    ) -> anyhow::Result<ToolResult> {
        self.executor()
            .execute_authorized_workflow_subcall_with_step(execution)
            .await
    }

    pub async fn execute_authorized_workflow_tool_with_step(
        &self,
        execution: AuthorizedWorkflowToolExecution,
    ) -> anyhow::Result<ToolResult> {
        self.executor()
            .execute_authorized_workflow_tool_with_step(execution)
            .await
    }

    #[cfg(test)]
    pub(crate) fn tool_circuits(&self) -> &ToolCircuitRegistry {
        &self.coordinator.core.tool_circuits
    }

    /// Reset per-tool failure gates through the execution owner without
    /// exposing the circuit registry or its synchronization policy.
    pub fn reset_tool_circuits(&self) {
        self.coordinator.core.tool_circuits.reset_all();
    }

    /// Inspection hook for integration tests. Production callers must execute
    /// through the facade so authorization, schema validation, circuit gates,
    /// retries, and cancellation policy cannot be bypassed.
    #[cfg(any(test, feature = "test-support"))]
    pub async fn get_tool(&self, name: &str) -> Option<ToolHandle> {
        self.executor().get_tool(name).await
    }

    pub async fn observation_text(&self, tool_name: &str, result: &ToolResult) -> String {
        self.executor().observation_text(tool_name, result).await
    }
}

impl AuthorizedExecutor<'_> {
    #[cfg(any(test, feature = "test-support"))]
    pub async fn execute_tool(
        &self,
        session_id: Option<&str>,
        tool_name: &str,
        input: Value,
        cancel: CancellationToken,
    ) -> anyhow::Result<ToolResult> {
        self.execute_tool_with_step(session_id, tool_name, input, cancel, None)
            .await
    }

    /// Like [`Self::execute_tool`], but also injects the pre-minted `step-*`
    /// id so tools that stream live output (shell) can key `agent:tool_output`
    /// events to the matching chat card.
    #[cfg(any(test, feature = "test-support"))]
    pub async fn execute_tool_with_step(
        &self,
        session_id: Option<&str>,
        tool_name: &str,
        input: Value,
        cancel: CancellationToken,
        step_id: Option<&str>,
    ) -> anyhow::Result<ToolResult> {
        self.execute_tool_inner(ExecutionRequest {
            session_id: session_id.map(str::to_owned),
            tool_name: tool_name.to_owned(),
            input,
            cancel,
            step_id: step_id.map(str::to_owned),
            authorization_proof: None,
            surface: ExecutionSurface::Model,
            codemode_subcalls: None,
            workflow_subcalls: None,
        })
        .await
    }

    pub async fn execute_authorized_tool_with_step(
        &self,
        session_id: Option<&str>,
        tool_name: &str,
        input: Value,
        cancel: CancellationToken,
        step_id: Option<&str>,
        proof: crate::AuthorizationExecutionProof,
    ) -> anyhow::Result<ToolResult> {
        self.execute_tool_inner(ExecutionRequest {
            session_id: session_id.map(str::to_owned),
            tool_name: tool_name.to_owned(),
            input,
            cancel,
            step_id: step_id.map(str::to_owned),
            authorization_proof: Some(proof),
            surface: ExecutionSurface::Model,
            codemode_subcalls: None,
            workflow_subcalls: None,
        })
        .await
    }

    pub async fn execute_authorized_codemode_tool_with_step(
        &self,
        execution: AuthorizedCodeModeToolExecution,
    ) -> anyhow::Result<ToolResult> {
        self.execute_tool_inner(ExecutionRequest {
            session_id: execution.session_id,
            tool_name: execution.tool_name,
            input: execution.input,
            cancel: execution.cancel,
            step_id: execution.step_id,
            authorization_proof: Some(execution.proof),
            surface: ExecutionSurface::Model,
            codemode_subcalls: Some(execution.subcalls),
            workflow_subcalls: None,
        })
        .await
    }

    pub async fn execute_authorized_codemode_subcall_with_step(
        &self,
        execution: AuthorizedCodeModeSubcallExecution,
    ) -> anyhow::Result<ToolResult> {
        self.execute_tool_inner(ExecutionRequest {
            session_id: execution.session_id,
            tool_name: execution.tool_name,
            input: execution.input,
            cancel: execution.cancel,
            step_id: execution.step_id,
            authorization_proof: Some(execution.proof),
            surface: ExecutionSurface::CodeModeSubcall,
            codemode_subcalls: None,
            workflow_subcalls: None,
        })
        .await
    }

    pub async fn execute_authorized_workflow_subcall_with_step(
        &self,
        execution: AuthorizedWorkflowSubcallExecution,
    ) -> anyhow::Result<ToolResult> {
        self.execute_tool_inner(ExecutionRequest {
            session_id: execution.session_id,
            tool_name: execution.tool_name,
            input: execution.input,
            cancel: execution.cancel,
            step_id: execution.step_id,
            authorization_proof: Some(execution.proof),
            surface: ExecutionSurface::WorkflowSubcall,
            codemode_subcalls: None,
            workflow_subcalls: None,
        })
        .await
    }

    pub async fn execute_authorized_workflow_tool_with_step(
        &self,
        execution: AuthorizedWorkflowToolExecution,
    ) -> anyhow::Result<ToolResult> {
        self.execute_tool_inner(ExecutionRequest {
            session_id: execution.session_id,
            tool_name: execution.tool_name,
            input: execution.input,
            cancel: execution.cancel,
            step_id: execution.step_id,
            authorization_proof: Some(execution.proof),
            surface: ExecutionSurface::Model,
            codemode_subcalls: None,
            workflow_subcalls: Some(execution.subcalls),
        })
        .await
    }

    async fn execute_tool_inner(&self, request: ExecutionRequest) -> anyhow::Result<ToolResult> {
        let ExecutionRequest {
            session_id,
            tool_name,
            input,
            cancel,
            step_id,
            authorization_proof,
            surface: execution_surface,
            codemode_subcalls,
            workflow_subcalls,
        } = request;
        let session_id = session_id.as_deref();
        let tool_name = tool_name.as_str();
        let step_id = step_id.as_deref();
        if !self
            .tools
            .coordinator
            .core
            .tool_circuits
            .allow_request(tool_name)
        {
            tracing::warn!("tool circuit breaker open; fast-failing");
            return Err(anyhow::Error::new(StructuredToolError::new(
                format!(
                    "tool '{}' is temporarily unavailable (circuit breaker open)",
                    tool_name
                ),
                ToolErrorMetadata::transient(),
            )));
        }

        if !self.tools.tool_enabled(tool_name).await {
            tracing::warn!("tool is disabled");
            return Err(anyhow::Error::new(StructuredToolError::new(
                format!("tool '{}' is disabled", tool_name),
                ToolErrorMetadata {
                    class: ToolErrorClass::Permission,
                    outcome: ToolExecutionOutcome::Failed,
                    retryability: ToolRetryability::NotRetryable,
                },
            )));
        }

        let tool = match execution_surface {
            ExecutionSurface::Model => self.tools.get_tool_for_session(session_id, tool_name).await,
            ExecutionSurface::CodeModeSubcall => {
                self.tools
                    .get_codemode_subcall_tool_for_session(session_id, tool_name)
                    .await
            }
            ExecutionSurface::WorkflowSubcall => {
                self.tools
                    .get_workflow_subcall_tool_for_session(session_id, tool_name)
                    .await
            }
        }
        .ok_or_else(|| {
            anyhow::Error::new(StructuredToolError::new(
                format!("tool '{}' not found in registry", tool_name),
                ToolErrorMetadata::other(),
            ))
        })?;

        // Private fields (`_session_id` / `_step_id` / `_idempotency_key`) are never trusted from
        // the LLM or scheduled tool_args: always strip first, validate the
        // LLM-facing input, then re-inject only caller-supplied values.
        // Declared via `Tool::requires_session_id` / `supports_live_output`.
        let authorization_input = input.clone();
        let mut exec_input = input;
        tool_contract::strip_private_tool_fields(&mut exec_input);
        if let Err(error) = tool.validate_input(&exec_input) {
            return Ok(ToolResult::failed_with_class(
                Value::Null,
                error.to_string(),
                ToolErrorClass::Validation,
            ));
        }
        if let Some(obj) = exec_input.as_object_mut() {
            let want_session = tool.requires_session_id() || tool.supports_live_output();
            if want_session && let Some(tid) = session_id {
                obj.insert("_session_id".into(), serde_json::json!(tid));
            }
            if tool.supports_live_output()
                && let Some(sid) = step_id.filter(|s| !s.is_empty())
            {
                obj.insert("_step_id".into(), serde_json::json!(sid));
            }
        }
        let platform = self.tools.coordinator.runtime.platform().await;
        let settings = &platform.tool_settings;
        let configured = settings
            .get(tool_name)
            .or_else(|| {
                tool_name
                    .split('.')
                    .next()
                    .and_then(|root| settings.get(root))
            })
            .cloned();
        let cfg = configured.clone().unwrap_or_default();
        // A settings entry refines only fields explicitly configured. In
        // particular, `None` must preserve operation-specific intrinsic
        // timeouts instead of silently replacing them with 30 seconds.
        let timeout_secs = cfg
            .timeout_secs
            .unwrap_or_else(|| tool.timeout_secs_for(&exec_input));
        let max_retries = configured
            .as_ref()
            .and_then(|c| c.max_retries)
            .unwrap_or_else(|| tool.default_max_retries());
        let backoff_secs = configured
            .as_ref()
            .and_then(|c| c.retry_backoff_secs)
            .unwrap_or_else(|| tool.default_retry_backoff_secs());
        let idempotency = tool.idempotency(&exec_input);

        // Keep tool-local retries bounded even when a persisted settings file
        // contains an accidentally large value. Agent-level retries have a
        // separate budget in haven-agent; both use the shared recovery policy.
        let max_attempts = 1 + max_retries.min(haven_common::config::MAX_TOOL_RETRIES);
        let jitter = if backoff_secs == 0 {
            RetryJitter::None
        } else {
            RetryJitter::Additive(Duration::from_millis(250))
        };
        let recovery = RecoveryPolicy::new(
            Some(max_attempts as u32),
            None,
            BackoffPolicy::new(
                Duration::from_secs(backoff_secs),
                2,
                Duration::from_secs(30),
            )
            .with_jitter(jitter),
        );
        let mut retry_delay: Option<Duration> = None;
        for attempt in 0..max_attempts {
            if let Some(delay) = retry_delay.take() {
                tracing::debug!(
                    attempt,
                    delay_ms = delay.as_millis() as u64,
                    "waiting before idempotent tool retry"
                );
                tokio::select! {
                    _ = tokio::time::sleep(delay) => {},
                    _ = cancel.cancelled() => {
                        return Ok(ToolResult::cancelled("tool execution cancelled during retry backoff"));
                    },
                }
            }
            if cancel.is_cancelled() {
                return Ok(ToolResult::cancelled("tool execution cancelled"));
            }

            let attempt_tool = match execution_surface {
                ExecutionSurface::Model => tool.clone(),
                ExecutionSurface::CodeModeSubcall => {
                    let Some(current) = self
                        .tools
                        .get_codemode_subcall_tool_for_session(session_id, tool_name)
                        .await
                    else {
                        return Ok(ToolResult::failed_with_class(
                            Value::Null,
                            "MCP capability is no longer enabled for CodeMode",
                            ToolErrorClass::Permission,
                        ));
                    };
                    if let Err(error) = current.validate_input(&exec_input) {
                        return Ok(ToolResult::failed_with_class(
                            Value::Null,
                            error.to_string(),
                            ToolErrorClass::Validation,
                        ));
                    }
                    current
                }
                ExecutionSurface::WorkflowSubcall => {
                    let Some(current) = self
                        .tools
                        .get_workflow_subcall_tool_for_session(session_id, tool_name)
                        .await
                    else {
                        return Ok(ToolResult::failed_with_class(
                            Value::Null,
                            "MCP capability is no longer callable by Workflow",
                            ToolErrorClass::Permission,
                        ));
                    };
                    if let Err(error) = current.validate_input(&exec_input) {
                        return Ok(ToolResult::failed_with_class(
                            Value::Null,
                            error.to_string(),
                            ToolErrorClass::Validation,
                        ));
                    }
                    current
                }
            };

            let mut authorization_target_binding = None;
            let mut authorization_context = crate::ToolExecutionContext::default();
            if let Some(proof) = authorization_proof.as_ref() {
                let request = match execution_surface {
                    ExecutionSurface::Model => {
                        self.tools
                            .resolve_authorization_request(
                                session_id,
                                tool_name,
                                &authorization_input,
                            )
                            .await
                    }
                    ExecutionSurface::CodeModeSubcall => {
                        self.tools
                            .resolve_codemode_subcall_authorization_request(
                                session_id,
                                tool_name,
                                &authorization_input,
                            )
                            .await
                    }
                    ExecutionSurface::WorkflowSubcall => {
                        self.tools
                            .resolve_workflow_subcall_authorization_request(
                                session_id,
                                tool_name,
                                &authorization_input,
                            )
                            .await
                    }
                }
                .map_err(anyhow::Error::msg)?;
                authorization_context = match self
                    .tools
                    .coordinator
                    .core
                    .authorization
                    .execution_context_for_proof(&request, proof)
                    .await
                {
                    Ok(context) => context,
                    Err(reason) => {
                        return Ok(ToolResult::failed_with_class(
                            Value::Null,
                            format!("operation authorization is no longer valid: {reason}"),
                            ToolErrorClass::Permission,
                        ));
                    }
                };
                authorization_context =
                    authorization_context.with_call_identity(session_id, step_id);
                authorization_target_binding = request.target_binding;
            }

            let execution = if let Some(subcalls) = codemode_subcalls.clone() {
                attempt_tool
                    .execute_with_codemode_subcalls_timeout(
                        exec_input.clone(),
                        cancel.clone(),
                        timeout_secs,
                        authorization_target_binding.as_deref(),
                        subcalls,
                    )
                    .await
            } else if let Some(subcalls) = workflow_subcalls.clone() {
                attempt_tool
                    .execute_with_workflow_subcalls_timeout(
                        exec_input.clone(),
                        cancel.clone(),
                        timeout_secs,
                        authorization_target_binding.as_deref(),
                        subcalls,
                    )
                    .await
            } else {
                attempt_tool
                    .execute_with_authorization_context_timeout(
                        exec_input.clone(),
                        cancel.clone(),
                        timeout_secs,
                        authorization_target_binding.as_deref(),
                        &authorization_context,
                    )
                    .await
            };
            let mut result = match execution {
                Ok(result) => result,
                Err(e) => {
                    let message = e.to_string();
                    // Cancellation is a control-plane fact owned by the
                    // token, not a substring in an arbitrary tool error. A
                    // normal tool failure such as "cancelled request was
                    // rejected" must remain Failed so retry/telemetry do not
                    // treat it as an externally cancelled run.
                    if cancel.is_cancelled() {
                        ToolResult::cancelled(message)
                    } else {
                        let metadata = attempt_tool.error_metadata(&e);
                        ToolResult::failed_with_metadata(Value::Null, message.clone(), metadata)
                    }
                }
            };
            result.attempts = attempt + 1;
            if result.success {
                self.tools
                    .coordinator
                    .core
                    .tool_circuits
                    .record_success(tool_name);
                // Attach the tool's declared side-channel signals (ask
                // question / notify toast) BEFORE returning.
                result.signals = attempt_tool.signals(&result.output);
                return Ok(result);
            }

            let signal = if result.outcome == ToolExecutionOutcome::Cancelled {
                RecoverySignal::Cancelled
            } else if result.outcome == ToolExecutionOutcome::TimedOutUnknown {
                RecoverySignal::OutcomeUnknown
            } else if matches!(idempotency, OperationIdempotency::Idempotent)
                && retryable_result(&result)
            {
                RecoverySignal::Retryable { retry_after: None }
            } else {
                RecoverySignal::PermanentFailure
            };
            if let RecoveryDecision::Retry { delay, .. } = recovery.decide(
                attempt + 1,
                signal,
                std::time::Instant::now(),
                Self::tool_retry_jitter_sample(tool_name, attempt + 1),
            ) {
                tracing::warn!(
                    attempt = result.attempts,
                    max_attempts,
                    outcome = ?result.outcome,
                    "idempotent tool attempt failed; retrying"
                );
                retry_delay = Some(delay);
                continue;
            }
            self.tools
                .coordinator
                .core
                .tool_circuits
                .record_failure(tool_name);
            annotate_idempotency(&mut result, idempotency);
            return Ok(result);
        }
        self.tools
            .coordinator
            .core
            .tool_circuits
            .record_failure(tool_name);
        Ok(ToolResult::failed(
            Value::Null,
            format!("tool '{}' retries exhausted", tool_name),
        ))
    }

    fn tool_retry_jitter_sample(tool_name: &str, completed_attempts: u32) -> u32 {
        use std::hash::{Hash, Hasher};

        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        tool_name.hash(&mut hasher);
        completed_attempts.hash(&mut hasher);
        let jitter_millis = hasher.finish() % 250;
        ((jitter_millis * u64::from(u32::MAX)) / 250) as u32
    }

    #[cfg(any(test, feature = "test-support"))]
    pub(crate) async fn get_tool(&self, name: &str) -> Option<ToolHandle> {
        if let Some(tool) = self
            .tools
            .coordinator
            .core
            .operations
            .installed
            .get(name)
            .await
        {
            return Some(tool);
        }
        self.tools
            .coordinator
            .core
            .operations
            .deferred
            .get(name)
            .await
    }

    /// Apply the configured per-tool/global observation cap to the stable
    /// ToolResult summary. Agent, step persistence and resume all consume this
    /// exact helper so an adapter cannot create a longer recovery observation.
    pub async fn observation_text(&self, tool_name: &str, result: &ToolResult) -> String {
        let platform = self.tools.coordinator.runtime.platform().await;
        let limits = &platform.context_limits;
        let settings = &platform.tool_settings;
        let cap = settings
            .get(tool_name)
            .or_else(|| {
                tool_name
                    .split('.')
                    .next()
                    .and_then(|root| settings.get(root))
            })
            .and_then(|config| config.max_output_chars)
            .unwrap_or(limits.max_observation_chars);
        OutputBudget::new(cap).observe(result)
    }
}
