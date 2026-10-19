use super::transcript::{ObservationCard, ToolCallCard, TranscriptWrite};
use super::{ReActEngine, StepCtx};
use crate::session::ToolStepMetadata;
use crate::types::AgentToolCall;
use haven_common::tools::{ToolErrorClass, ToolOperationScope};
use haven_common::types::{CanonicalToolCall, RiskLevel, new_id};
use haven_tools::{ToolResult, is_silent_tool_call};
use serde_json::{Value, json};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

impl ReActEngine {
    /// Execute a Skill preview as a committed step on its owning session.
    /// The caller must hold a managed run lease and supply its live emitter.
    pub(crate) async fn execute_managed_skill_step(
        &self,
        session_id: &str,
        skill_name: &str,
        params: &Value,
        emitter: Arc<dyn crate::event::AgentEventEmitter>,
        cancel: CancellationToken,
    ) -> anyhow::Result<(String, ToolResult)> {
        let tool_name = haven_tools::skill_tool_name(skill_name);
        let tool_input = json!({ "params": params });
        let catalog = self.build_tool_catalog_for_session(session_id).await;
        anyhow::ensure!(
            catalog.get(&tool_name).is_some(),
            "Skill is unavailable in the active tool catalog"
        );
        if let Some(validation) = catalog.validate_input(&tool_name, &tool_input) {
            validation?;
        }
        let policy = catalog.operation_policy(&tool_name, &tool_input);
        anyhow::ensure!(
            policy.scope == ToolOperationScope::Session,
            "Skill preview requires session-scoped tool authorization"
        );

        let projection = self
            .event_store
            .session_resume_projection(session_id)
            .await?;
        let step_number = projection
            .steps
            .iter()
            .map(|step| step.step_number)
            .max()
            .map(|current| {
                current
                    .checked_add(1)
                    .ok_or_else(|| anyhow::anyhow!("session step number space is exhausted"))
            })
            .transpose()?
            .unwrap_or(1);
        let step_id = new_id("step");
        let tool_call_id = new_id("call");
        let run_id = self.next_run_id();
        let ctx = StepCtx {
            session_id: session_id.to_string(),
            step_number,
            run_id,
            emitter,
        };
        let agent_tool_call = AgentToolCall {
            tool_name: tool_name.clone(),
            tool_input: tool_input.clone(),
            is_final: false,
            tool_call_id: Some(tool_call_id.clone()),
        };
        let canonical_call = CanonicalToolCall {
            tool_call_id: tool_call_id.clone(),
            name: tool_name.clone(),
            arguments: tool_input.clone(),
        };
        let tool_call_card = ToolCallCard {
            tool_name: tool_name.clone(),
            tool_input: tool_input.clone(),
            tool_call_id: Some(tool_call_id.clone()),
            step_id: step_id.clone(),
            tool_index: 0,
            suppress_streamed_thought: true,
            is_high_risk: policy.risk_level != RiskLevel::Safe,
            silent: is_silent_tool_call(&tool_name, &tool_input),
        };
        let silent = tool_call_card.silent;
        self.commit_managed_tool_write(
            &ctx,
            TranscriptWrite::ToolCall {
                text: String::new(),
                tool_calls: vec![canonical_call],
                reasoning: None,
                web_search_calls: Vec::new(),
                thinking_blocks: Vec::new(),
                tool_call_cards: vec![tool_call_card],
                persist_text_id: None,
            },
            cancel.child_token(),
        )
        .await?;

        let result = match self
            .executor
            .execute_step_with_identity_and_metadata_and_cancel(
                session_id,
                &tool_name,
                tool_input.clone(),
                step_number,
                0,
                Some(&tool_call_id),
                &step_id,
                ToolStepMetadata {
                    is_high_risk: policy.risk_level != RiskLevel::Safe,
                    silent,
                },
                cancel.child_token(),
            )
            .await
        {
            Ok(result) => result,
            Err(error) => {
                let safe_error = haven_common::error::sanitize_error_text(&error.to_string());
                self.executor
                    .finish_interrupted_step(
                        session_id,
                        &tool_name,
                        &tool_input,
                        step_number,
                        &step_id,
                        &safe_error,
                    )
                    .await;
                ToolResult::failed_with_class(Value::Null, safe_error, ToolErrorClass::Other)
            }
        };
        let observation = self.executor.observation_text(&tool_name, &result).await;
        let result_envelope = result.envelope(policy.idempotency);
        let observation_card = ObservationCard {
            tool_name: tool_name.clone(),
            tool_call_id: Some(tool_call_id.clone()),
            step_id: step_id.clone(),
            tool_index: 0,
            silent,
            ask_options: result.signals.ask_options.clone(),
            idempotency: policy.idempotency,
            operation_scope: policy.scope,
            renderer: catalog
                .manifest(&tool_name)
                .map(|manifest| manifest.presentation.renderer)
                .unwrap_or_else(|| "skill".into()),
            result_envelope,
        };
        self.commit_managed_tool_write(
            &ctx,
            TranscriptWrite::ToolResult {
                canonical_observation: observation.clone(),
                history_observation: observation,
                tool_call_id: Some(tool_call_id),
                tool_call: agent_tool_call,
                tool_index: 0,
                step_id: step_id.clone(),
                observation_card: Some(Box::new(observation_card)),
            },
            cancel.child_token(),
        )
        .await?;

        Ok((step_id, result))
    }
}
