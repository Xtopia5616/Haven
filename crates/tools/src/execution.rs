use super::*;

/// Pure execution entry: admit the call, validate it, run the handler, and
/// classify the outcome.
///
/// `ToolsManager` only forwards to this type. Interactive authorization is
/// assembled here and decided by the caller before `execute`, because a
/// missing confirmation receipt must fail closed without blocking inside the
/// tool future.
pub(crate) struct AuthorizedExecutor<'a> {
    tools: &'a ToolsManager,
}

impl ToolsManager {
    pub(crate) fn executor(&self) -> AuthorizedExecutor<'_> {
        AuthorizedExecutor { tools: self }
    }

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

    pub fn tool_circuits(&self) -> &ToolCircuitRegistry {
        &self.coordinator.core.tool_circuits
    }

    pub async fn get_tool(&self, name: &str) -> Option<ToolBox> {
        self.executor().get_tool(name).await
    }

    pub async fn observation_text(&self, tool_name: &str, result: &ToolResult) -> String {
        self.executor().observation_text(tool_name, result).await
    }
}

impl AuthorizedExecutor<'_> {
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
    pub async fn execute_tool_with_step(
        &self,
        session_id: Option<&str>,
        tool_name: &str,
        input: Value,
        cancel: CancellationToken,
        step_id: Option<&str>,
    ) -> anyhow::Result<ToolResult> {
        if !self
            .tools
            .coordinator
            .core
            .tool_circuits
            .allow_request(tool_name)
        {
            tracing::warn!("tool '{}' circuit breaker open — fast-failing", tool_name);
            return Err(anyhow::Error::new(StructuredToolError::new(
                format!(
                    "tool '{}' is temporarily unavailable (circuit breaker open)",
                    tool_name
                ),
                ToolErrorMetadata::transient(),
            )));
        }

        if !self.tools.tool_enabled(tool_name).await {
            tracing::warn!("tool '{}' is disabled", tool_name);
            return Err(anyhow::Error::new(StructuredToolError::new(
                format!("tool '{}' is disabled", tool_name),
                ToolErrorMetadata {
                    class: ToolErrorClass::Permission,
                    outcome: ToolExecutionOutcome::Failed,
                    retryability: ToolRetryability::NotRetryable,
                },
            )));
        }

        let tool = self
            .tools
            .get_tool_for_session(session_id, tool_name)
            .await
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
            let want_session =
                tool.requires_session_id() || (tool.supports_live_output() && step_id.is_some());
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
        // separate budget in haven-agent.
        let max_attempts = 1 + max_retries.min(8);
        for attempt in 0..max_attempts {
            if attempt > 0 {
                let delay = tool_retry_delay(tool_name, backoff_secs, attempt);
                tracing::debug!(
                    tool = %tool_name,
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

            let mut result = match tool
                .execute_with_timeout(exec_input.clone(), cancel.clone(), timeout_secs)
                .await
            {
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
                        let metadata = tool.error_metadata(&e);
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
                result.signals = tool.signals(&result.output);
                return Ok(result);
            }

            let can_retry = matches!(idempotency, OperationIdempotency::Idempotent)
                && attempt + 1 < max_attempts
                && retryable_result(&result);
            if can_retry {
                tracing::warn!(
                    tool = %tool_name,
                    attempt = result.attempts,
                    max_attempts,
                    outcome = ?result.outcome,
                    "idempotent tool attempt failed; retrying"
                );
                continue;
            }
            self.tools
                .coordinator
                .core
                .tool_circuits
                .record_failure(tool_name);
            annotate_retry_safety(&mut result, idempotency);
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

    pub async fn get_tool(&self, name: &str) -> Option<ToolBox> {
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
