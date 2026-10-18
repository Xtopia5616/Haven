use super::*;

pub(super) struct GatedToolExecutionRequest<'a> {
    pub(super) session_id: Option<&'a str>,
    pub(super) tool_name: &'a str,
    pub(super) input: Value,
    pub(super) cancel: CancellationToken,
    pub(super) receipt: Option<haven_tools::ConfirmationReceipt>,
    pub(super) step_id: Option<&'a str>,
    pub(super) nested_confirmation: Option<haven_tools::NestedConfirmationRequest>,
    pub(super) tool_run_id: Option<&'a str>,
}

impl SessionSupervisor {
    pub async fn observation_text(&self, tool_name: &str, result: &ToolResult) -> String {
        self.observation_port
            .observation_text(tool_name, result)
            .await
    }

    /// Execute a tool step. `step_id` is the pre-minted `step-*` id the frontend's
    /// live tool card already uses; the persisted step row reuses it so the live
    /// card and the resume badge are one entity. The pending row is normally
    /// created by [`Self::begin_tool_step`] at ToolCall emit; this method
    /// ensures + completes it after the tool finishes.
    #[allow(clippy::too_many_arguments)]
    pub async fn execute_step(
        &self,
        session_id: &str,
        tool_name: &str,
        input: Value,
        step_number: u32,
        step_id: &str,
    ) -> anyhow::Result<ToolResult> {
        self.execute_step_with_identity(session_id, tool_name, input, step_number, 0, None, step_id)
            .await
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn execute_step_with_identity(
        &self,
        session_id: &str,
        tool_name: &str,
        input: Value,
        step_number: u32,
        tool_index: u32,
        tool_call_id: Option<&str>,
        step_id: &str,
    ) -> anyhow::Result<ToolResult> {
        self.execute_step_inner(
            session_id,
            tool_name,
            input,
            step_number,
            tool_index,
            tool_call_id,
            step_id,
            None,
            None,
            None,
            None,
        )
        .await
    }

    /// Metadata-preserving variant used by a ReAct turn. The caller supplies
    /// the turn-scoped cancellation token so a deadline can stop a tool even
    /// though the session itself remains live for recovery.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn execute_step_with_identity_and_metadata_and_cancel(
        &self,
        session_id: &str,
        tool_name: &str,
        input: Value,
        step_number: u32,
        tool_index: u32,
        tool_call_id: Option<&str>,
        step_id: &str,
        metadata: ToolStepMetadata,
        cancel: tokio_util::sync::CancellationToken,
    ) -> anyhow::Result<ToolResult> {
        self.execute_step_with_identity_and_metadata_and_cancel_and_nested(
            session_id,
            tool_name,
            input,
            step_number,
            tool_index,
            tool_call_id,
            step_id,
            metadata,
            cancel,
            None,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn execute_step_with_identity_and_metadata_and_cancel_and_nested(
        &self,
        session_id: &str,
        tool_name: &str,
        input: Value,
        step_number: u32,
        tool_index: u32,
        tool_call_id: Option<&str>,
        step_id: &str,
        metadata: ToolStepMetadata,
        cancel: tokio_util::sync::CancellationToken,
        nested_confirmation: Option<haven_tools::NestedConfirmationRequest>,
    ) -> anyhow::Result<ToolResult> {
        self.execute_step_inner(
            session_id,
            tool_name,
            input,
            step_number,
            tool_index,
            tool_call_id,
            step_id,
            None,
            nested_confirmation,
            Some(metadata),
            Some(cancel),
        )
        .await
    }

    /// Like [`execute_step`], but skips the blocking confirm wait when
    /// `receipt` is supplied only when the authorization engine approved the
    /// exact invocation earlier (Phase 5 / E3 resume after pause-confirm).
    pub async fn execute_step_preconfirmed(
        &self,
        session_id: &str,
        tool_name: &str,
        input: Value,
        step_number: u32,
        step_id: &str,
        receipt: haven_tools::ConfirmationReceipt,
    ) -> anyhow::Result<ToolResult> {
        self.execute_step_preconfirmed_with_identity(
            session_id,
            tool_name,
            input,
            step_number,
            0,
            None,
            step_id,
            receipt,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn execute_step_preconfirmed_with_identity(
        &self,
        session_id: &str,
        tool_name: &str,
        input: Value,
        step_number: u32,
        tool_index: u32,
        tool_call_id: Option<&str>,
        step_id: &str,
        receipt: haven_tools::ConfirmationReceipt,
    ) -> anyhow::Result<ToolResult> {
        self.execute_step_inner(
            session_id,
            tool_name,
            input,
            step_number,
            tool_index,
            tool_call_id,
            step_id,
            Some(receipt),
            None,
            None,
            None,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn execute_step_preconfirmed_with_identity_and_metadata_and_cancel_and_nested(
        &self,
        session_id: &str,
        tool_name: &str,
        input: Value,
        step_number: u32,
        tool_index: u32,
        tool_call_id: Option<&str>,
        step_id: &str,
        receipt: Option<haven_tools::ConfirmationReceipt>,
        nested_confirmation: Option<haven_tools::NestedConfirmationRequest>,
        metadata: ToolStepMetadata,
        cancel: tokio_util::sync::CancellationToken,
    ) -> anyhow::Result<ToolResult> {
        self.execute_step_inner(
            session_id,
            tool_name,
            input,
            step_number,
            tool_index,
            tool_call_id,
            step_id,
            receipt,
            nested_confirmation,
            Some(metadata),
            Some(cancel),
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn execute_step_inner(
        &self,
        session_id: &str,
        tool_name: &str,
        input: Value,
        step_number: u32,
        tool_index: u32,
        tool_call_id: Option<&str>,
        step_id: &str,
        receipt: Option<haven_tools::ConfirmationReceipt>,
        nested_confirmation: Option<haven_tools::NestedConfirmationRequest>,
        tool_step_metadata: Option<ToolStepMetadata>,
        cancel_override: Option<tokio_util::sync::CancellationToken>,
    ) -> anyhow::Result<ToolResult> {
        let tool_call_id = tool_call_id.map(str::to_string);
        let input_bytes = serde_json::to_vec(&input)
            .map(|serialized| serialized.len())
            .unwrap_or(0);
        tracing::debug!(
            session_id = %session_id,
            step_number,
            input_field_count = input.as_object().map_or(0, |object| object.len()),
            input_bytes,
            receipt_present = receipt.is_some(),
            "execute_step"
        );
        {
            let actor = self.actor_for(session_id).await;
            // Phase 7 / E4: only Running may execute tools. Missing session
            // (end_session already removed the entry) must fail closed — never
            // treat absence as permission to run.
            let refuse = match actor {
                None => Some(format!(
                    "execute_step: session {} not in working set; refusing to execute tool '{}'",
                    session_id, tool_name
                )),
                Some(actor) => {
                    let prev = actor.snapshot().await.map(|session| session.status);
                    let Some(prev) = prev else {
                        return Err(anyhow::anyhow!(
                            "execute_step: session {} actor stopped",
                            session_id
                        ));
                    };
                    if !matches!(prev, SessionStatus::Running) {
                        Some(format!(
                            "execute_step: session {} is {}; refusing to execute tool '{}'",
                            session_id,
                            prev.as_str(),
                            tool_name
                        ))
                    } else {
                        None
                    }
                }
            };
            if let Some(err) = refuse {
                if let Some(metadata) = tool_step_metadata {
                    self.finish_interrupted_step_with_identity_and_metadata(
                        session_id,
                        tool_name,
                        &input,
                        step_number,
                        tool_index,
                        tool_call_id.as_deref(),
                        step_id,
                        &err,
                        metadata,
                    )
                    .await;
                } else {
                    self.finish_interrupted_step_with_identity(
                        session_id,
                        tool_name,
                        &input,
                        step_number,
                        tool_index,
                        tool_call_id.as_deref(),
                        step_id,
                        &err,
                    )
                    .await;
                }
                return Err(anyhow::anyhow!(err));
            }
        }

        let cancel = match cancel_override {
            Some(cancel) => cancel,
            None => self.cancellation_token(session_id).await,
        };
        let tool_step_request = ToolStepRequest {
            session_id,
            tool_name,
            input: &input,
            step_number,
            tool_index,
            tool_call_id: tool_call_id.as_deref(),
            step_id,
        };
        let tool_step_context = match tool_step_metadata {
            Some(metadata) => ToolStepContext::new_with_metadata(tool_step_request, metadata),
            None => self.tool_step_context(tool_step_request).await,
        };
        self.start_running_tool_step_context(tool_step_context)
            .await?;
        let gated = match self
            .execute_gated_with_nested_confirmation(GatedToolExecutionRequest {
                session_id: Some(session_id),
                tool_name,
                input: input.clone(),
                cancel: cancel.clone(),
                receipt,
                step_id: Some(step_id),
                nested_confirmation,
                tool_run_id: None,
            })
            .await
        {
            Ok(gated) => gated,
            Err(e) => {
                // Pending row was created at ToolCall emit; record the failure
                // so resume/resync does not rebuild an empty tool badge.
                let outcome = if cancel.is_cancelled() {
                    ToolStepOutcome::Unknown
                } else {
                    ToolStepOutcome::Failed
                };
                if let Some(metadata) = tool_step_metadata {
                    self.finish_step_with_outcome_and_metadata(
                        session_id,
                        tool_name,
                        &input,
                        step_number,
                        tool_index,
                        tool_call_id.as_deref(),
                        step_id,
                        &e.to_string(),
                        outcome,
                        metadata,
                    )
                    .await;
                } else {
                    self.finish_step_with_outcome(
                        session_id,
                        tool_name,
                        &input,
                        step_number,
                        tool_index,
                        tool_call_id.as_deref(),
                        step_id,
                        &e.to_string(),
                        outcome,
                    )
                    .await;
                }
                return Err(e);
            }
        };
        let ToolExecution {
            result,
            risk_level,
            confirmed,
        } = gated;
        if result.signals.nested_confirmation.is_some() {
            // Keep the parent step open while its nested confirmation waits.
            return Ok(result);
        }
        tracing::info!(
            session_id = %session_id,
            step_number,
            success = result.success,
            attempts = result.attempts,
            outcome = ?result.outcome,
            "execute_step result"
        );

        // Apply the tool's declared per-session side effects (MCP adapter
        // registration) instead of name-matching loader tools here —
        // a new tool with a side effect declares it via `Tool::registrations`
        // and nothing in this executor needs to change. Background ToolRun
        // bindings are applied after the running-set guard below (a ToolRun
        // spawned in a concurrently-rolled-back step must not attach past
        // the cleanup sweep). `registrations` is extracted ONCE: calling it
        // twice could yield divergent results for stateful tools, and the
        // variant split below is explicit rather than silently partitioned.
        let registrations = if result.success {
            self.execution
                .registrations(session_id, tool_name, &result.output)
                .await
        } else {
            Vec::new()
        };
        // Guard against rollback/cancel: if the session has been removed from the
        // running set while the tool was executing, skip persisting step records
        // that would otherwise write into a session after its lifecycle changed.
        if !self.is_run_in_flight(session_id).await {
            tracing::warn!(
                "execute_step: session {} left running set during tool execution; skipping step record",
                session_id
            );
            return Ok(result);
        }
        // Apply session-local registrations only after the terminal/rollback
        // fence above. A tool can finish successfully just as its session is
        // ended; registering its MCP overlay after that point would
        // leak tools into a dead session and let a late result mutate state.
        for reg in &registrations {
            match reg {
                haven_tools::ToolRegistration::McpServer(name) => {
                    self.session_tool_overlay_port
                        .register_mcp_for_session(session_id, name, None)
                        .await;
                }
                haven_tools::ToolRegistration::ToolRun(_) => {}
            }
        }
        // Tie a background ToolRun to its session so end/rollback can clean it up.
        // Applied only AFTER the running-set guard above passed (a rollback
        // racing this step may have removed the session); the registrations were
        // extracted once, before the guard.
        for reg in &registrations {
            if let haven_tools::ToolRegistration::ToolRun(tool_run_id) = reg {
                self.tool_runs.attach_session(tool_run_id, session_id).await;
            }
        }
        let obs = self
            .observation_port
            .observation_text(tool_name, &result)
            .await;
        let step_outcome = tool_step_outcome(&result);
        let persist_step_id = step_id.to_string();
        let tool_name_owned = tool_name.to_string();
        // The in-memory StepInfo reuses the persisted step row's step_id so the
        // live session state and the resume history reference the same step.
        if let Some(actor) = self.actor_for(session_id).await {
            actor
                .record_step(StepInfo {
                    step_id: persist_step_id.clone(),
                    step_number,
                    tool_name: tool_name_owned.clone(),
                    input: input.clone(),
                    output: Some(result.output.clone()),
                    status: step_outcome.status(),
                    risk_level,
                    confirmed,
                })
                .await;
        }
        // Row was normally created at ToolCall emit; ensure + complete covers
        // direct execute_step callers (tests) and races where begin failed.
        let tool_step = ToolStepContext::new(
            ToolStepRequest {
                session_id,
                tool_name,
                input: &input,
                step_number,
                tool_index,
                tool_call_id: tool_call_id.as_deref(),
                step_id,
            },
            risk_level,
        );
        self.store
            .ensure_and_finish_tool_step(tool_step.into_write(), confirmed, obs, step_outcome)
            .await
            .map_err(|error| anyhow::Error::new(ToolStepPersistenceError(error)))?;
        Ok(result)
    }

    /// Execute a tool through the safety gateway. The tool's risk level is
    /// checked against the configured threshold BEFORE anything runs; an
    /// operation at/above the threshold blocks on the user's confirmation
    /// (`interaction:requested` event + `resolve_confirmation`), and is aborted
    /// when the user declines or the session is cancelled. Returns a failed
    /// `ToolResult` for declined operations so the ReAct loop sees a normal
    /// tool failure the model can react to.
    pub async fn execute_gated(
        &self,
        session_id: Option<&str>,
        tool_name: &str,
        input: Value,
        cancel: CancellationToken,
        receipt: Option<haven_tools::ConfirmationReceipt>,
        step_id: Option<&str>,
    ) -> anyhow::Result<ToolExecution> {
        self.execute_gated_with_nested_confirmation(GatedToolExecutionRequest {
            session_id,
            tool_name,
            input,
            cancel,
            receipt,
            step_id,
            nested_confirmation: None,
            tool_run_id: None,
        })
        .await
    }

    pub(super) async fn execute_gated_with_nested_confirmation(
        &self,
        request: GatedToolExecutionRequest<'_>,
    ) -> anyhow::Result<ToolExecution> {
        let GatedToolExecutionRequest {
            session_id,
            tool_name,
            input,
            cancel,
            receipt,
            step_id,
            nested_confirmation,
            tool_run_id,
        } = request;
        let authorization_request = self
            .authorization
            .authorization_request(session_id, tool_name, &input)
            .await
            .map_err(anyhow::Error::msg)?;
        let risk_level = self
            .authorization
            .risk_level(session_id, tool_name, &input)
            .await
            .map_err(anyhow::Error::msg)?;
        let mut confirmed: Option<bool> = None;
        if let Some(receipt) = receipt.as_ref()
            && let Err(reason) = self
                .authorization
                .verify_receipt(&authorization_request, receipt)
                .await
        {
            tracing::warn!(
                session_id = ?session_id,
                reason_bytes = reason.len(),
                "confirmation receipt rejected; refusing to execute"
            );
            return Ok(ToolExecution {
                result: ToolResult {
                    success: false,
                    output: Value::Null,
                    error: Some(format!(
                        "The confirmation for operation '{}' is no longer valid ({reason}). The operation was not executed.",
                        tool_name
                    )),
                    error_class: Some(haven_common::tools::ToolErrorClass::Permission),
                    retryability: haven_common::tools::ToolRetryability::NotRetryable,
                    truncated: false,
                    outcome: haven_common::tools::ToolExecutionOutcome::Cancelled,
                    attempts: 1,
                    signals: haven_tools::ToolSignals::default(),
                    llm_usage: Vec::new(),
                },
                risk_level,
                confirmed: Some(false),
            });
        }
        let authorization_check = self
            .authorization
            .authorize_for_execution(&authorization_request)
            .await;
        let mut execution_proof = authorization_check.execution_proof;
        match authorization_check.decision {
            AuthorizationDecision::AutoApproved => {}
            AuthorizationDecision::Blocked { reason, .. } => {
                return Ok(ToolExecution {
                    result: ToolResult {
                        success: false,
                        output: Value::Null,
                        error: Some(format!(
                            "operation '{tool_name}' is blocked by the security policy ({reason}). Do NOT retry it — ask the user what to do instead or choose a different approach."
                        )),
                        error_class: Some(haven_common::tools::ToolErrorClass::Permission),
                        retryability: haven_common::tools::ToolRetryability::NotRetryable,
                        truncated: false,
                        outcome: haven_common::tools::ToolExecutionOutcome::Failed,
                        attempts: 1,
                        signals: haven_tools::ToolSignals::default(),
                        llm_usage: Vec::new(),
                    },
                    risk_level,
                    confirmed: Some(false),
                });
            }
            AuthorizationDecision::RequiresConfirmation { .. } => {
                // R2 / Phase 5 E3: never block inside the tool future.
                // Callers must pre-decide via pause-confirm or
                // `request_scheduled_confirm`. Missing receipt fails closed.
                match receipt {
                    Some(receipt) => {
                        confirmed = Some(true);
                        execution_proof = Some(
                            self.authorization
                                .confirm_for_execution(&authorization_request, &receipt)
                                .await
                                .map_err(anyhow::Error::msg)?,
                        );
                    }
                    None => {
                        tracing::warn!(
                            session_id = ?session_id,
                            "execute_gated RequiresConfirmation without a receipt; rejecting (R2 fail-closed)"
                        );
                        return Ok(ToolExecution {
                            result: ToolResult {
                                success: false,
                                output: Value::Null,
                                error: Some(format!(
                                    "The user REJECTED the operation '{}' (confirmation declined). Do NOT retry it — ask the user what to do instead or choose a different approach.",
                                    tool_name
                                )),
                                error_class: Some(haven_common::tools::ToolErrorClass::Permission),
                                retryability: haven_common::tools::ToolRetryability::NotRetryable,
                                truncated: false,
                                outcome: haven_common::tools::ToolExecutionOutcome::Cancelled,
                                attempts: 1,
                                signals: haven_tools::ToolSignals::default(),
                                llm_usage: Vec::new(),
                            },
                            risk_level,
                            confirmed: Some(false),
                        });
                    }
                }
            }
        }
        let execution_proof = execution_proof.ok_or_else(|| {
            anyhow::anyhow!("authorization decision did not issue an execution proof")
        })?;
        let codemode_subcalls = if tool_name == haven_tools::CODEMODE_RUN_TOOL_NAME {
            let Some(session_id) = session_id else {
                return Err(anyhow::anyhow!("CodeMode requires a live session context"));
            };
            let snapshot = self.tool_catalog.catalog_snapshot(session_id).await;
            let definitions = snapshot
                .get(tool_name)
                .and_then(|tool| tool.codemode_tool_definitions());
            let Some(definitions) = definitions.filter(|definitions| !definitions.is_empty())
            else {
                return Ok(ToolExecution {
                    result: ToolResult::failed_with_class(
                        Value::Null,
                        "CodeMode tools are no longer exposed in this session",
                        haven_common::tools::ToolErrorClass::Permission,
                    ),
                    risk_level,
                    confirmed: Some(false),
                });
            };
            Some(self.codemode_subcall_port_with_resume(
                session_id,
                definitions.into_iter().map(|definition| definition.name),
                tool_run_id.or(step_id),
                nested_confirmation.clone(),
            )?)
        } else {
            None
        };
        let workflow_subcalls = if tool_name == haven_tools::WORKFLOW_RUN_TOOL_NAME {
            let Some(session_id) = session_id else {
                return Err(anyhow::anyhow!("Workflow requires a live session context"));
            };
            let snapshot = self.tool_catalog.catalog_snapshot(session_id).await;
            let definitions = snapshot
                .get(tool_name)
                .and_then(|tool| tool.workflow_capability_definitions());
            let Some(definitions) = definitions.filter(|definitions| !definitions.is_empty())
            else {
                return Ok(ToolExecution {
                    result: ToolResult::failed_with_class(
                        Value::Null,
                        "Workflow capabilities are no longer exposed in this session",
                        haven_common::tools::ToolErrorClass::Permission,
                    ),
                    risk_level,
                    confirmed: Some(false),
                });
            };
            Some(self.workflow_subcall_port_with_resume(
                session_id,
                definitions.into_iter().map(|definition| definition.name),
                tool_run_id.or(step_id),
                nested_confirmation,
            )?)
        } else {
            None
        };
        let context = ToolExecutionContext {
            session_id: session_id.map(str::to_owned),
            tool_name: tool_name.to_owned(),
            input,
            cancel,
            step_id: step_id.map(str::to_owned),
            authorization_proof: Some(execution_proof),
        };
        let result = match (codemode_subcalls, workflow_subcalls) {
            (Some(subcalls), None) => self.execution.execute_codemode(context, subcalls).await?,
            (None, Some(subcalls)) => self.execution.execute_workflow(context, subcalls).await?,
            (None, None) => self.execution.execute(context).await?,
            (Some(_), Some(_)) => anyhow::bail!("tool cannot receive two nested execution ports"),
        };
        Ok(ToolExecution {
            result,
            risk_level,
            confirmed,
        })
    }

    pub(crate) async fn execute_scheduled_workflow_with_nested_confirmation(
        &self,
        session_id: Option<&str>,
        input: Value,
        cancel: CancellationToken,
        tool_run_id: &str,
    ) -> anyhow::Result<ToolExecution> {
        self.execute_gated_with_nested_confirmation(GatedToolExecutionRequest {
            session_id,
            tool_name: haven_tools::WORKFLOW_RUN_TOOL_NAME,
            input,
            cancel,
            receipt: None,
            step_id: None,
            nested_confirmation: None,
            tool_run_id: Some(tool_run_id),
        })
        .await
    }
}
