//! One model sampling turn in the ReAct runtime.
//!
//! A [`Turn`] is deliberately smaller than a run: it prepares context, calls
//! the model once (including response-policy retries), materializes the model
//! response, and either executes one tool batch or reaches a turn boundary.
//! The outer run owns the step budget and lifecycle transitions.

use super::effects::EffectBatch;
use super::response_cycle::{AcceptedResponse, ResponseCycleOutcome};
use super::stream_step::SearchContextOutcome;
use super::tool_batch_policy::ToolRetryBudget;
use super::turn_end::TurnEndInput;
use super::*;
use std::sync::Arc;
use tracing::Instrument;

/// Inputs owned by the outer run and borrowed by one model turn.
pub(super) struct TurnInput<'a> {
    pub(super) ctx: StepCtx,
    pub(super) state: &'a mut ReActState,
    pub(super) cancel: tokio_util::sync::CancellationToken,
    pub(super) deadline: super::r#loop::TurnDeadline,
    /// Whether this turn may stage a tool-failure retry for another turn.
    /// Computed by the run driver from the absolute run end.
    pub(super) allow_tool_retry: bool,
    pub(super) tool_retry_budget: &'a mut ToolRetryBudget,
    pub(super) cut_off_retries: &'a mut u32,
}

/// Stateless turn coordinator. All session-owned mutable state remains in the
/// borrowed [`ReActState`] and the `SessionActor`; this type only advances one
/// model/tool turn and returns an [`EffectBatch`] for the run driver to apply.
pub(super) struct TurnEngine<'a> {
    engine: &'a ReActEngine,
}

impl ReActEngine {
    pub(super) fn turn_engine(&self) -> TurnEngine<'_> {
        TurnEngine { engine: self }
    }
}

impl TurnEngine<'_> {
    pub(super) async fn run(&self, input: TurnInput<'_>) -> anyhow::Result<EffectBatch> {
        self.engine.run_turn_impl(input).await
    }
}

impl ReActEngine {
    /// Emit completed provider web-search items after the stream has been
    /// folded. Streaming providers already emitted lifecycle updates; this
    /// final pass attaches the compact result payload and covers providers
    /// whose search calls are only visible in the aggregate response.
    fn web_search_return_effects(
        session_id: &str,
        step_num: u32,
        run_id: u64,
        web_search_calls: &[serde_json::Value],
    ) -> Vec<crate::event::AgentEvent> {
        let mut events = Vec::new();
        for item in web_search_calls {
            let Some(result) = haven_llm::web_search_result_of(item) else {
                continue;
            };
            events.push(crate::event::AgentEvent::WebSearch {
                session_id: session_id.to_string(),
                phase: "completed".into(),
                step_number: step_num,
                run_id,
                call_id: item
                    .get("id")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string),
                action: item
                    .pointer("/action/type")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string),
                result: Some(result),
            });
        }
        events
    }

    async fn run_turn_impl(&self, input: TurnInput<'_>) -> anyhow::Result<EffectBatch> {
        let TurnInput {
            ctx,
            state,
            cancel,
            deadline,
            allow_tool_retry,
            tool_retry_budget: _tool_retry_budget,
            cut_off_retries,
        } = input;
        let session_id = &ctx.session_id;
        let step_num = ctx.step_num;
        deadline.ensure_remaining("turn start")?;
        self.metrics.increment(MetricsCounter::TurnStarts);

        // Context is collected once at the turn boundary and projected by the
        // single transcript writer. Hooks may compact or refresh the context,
        // but they never own queue reads or persistence.
        {
            let _timer = self.metrics.start(
                MetricsPhase::ContextInject,
                session_id,
                ctx.run_id,
                step_num,
            );
            self.inject_turn_start_context(&ctx, state)
                .instrument(tracing::info_span!("inject", session_id, step_num))
                .await?;
            deadline.ensure_remaining("context injection")?;
        }

        let before_step = self
            .hooks
            .before_step(self, &ctx, state, cancel.clone())
            .instrument(tracing::info_span!("before_step", session_id, step_num))
            .await?;
        deadline.ensure_remaining("before-step hooks")?;

        if let Some(memory_trigger) = before_step.memory_trigger {
            crate::memory_trigger::append_memory_trigger_nonfatal(
                self.event_store.clone(),
                session_id,
                memory_trigger,
                cancel.clone(),
            )
            .await;
        }

        // Build one immutable provider projection. Durable canonical state is
        // never used as a scratch buffer by retries or provider repairs.
        let retry_nudge = state.take_retry_nudge();
        let cached_message_tokens = {
            let _timer = self.metrics.start(
                MetricsPhase::TokenEstimate,
                session_id,
                ctx.run_id,
                step_num,
            );
            state.estimate_canonical_tokens()
        };
        let request_context = {
            let _timer = self.metrics.start(
                MetricsPhase::RequestContext,
                session_id,
                ctx.run_id,
                step_num,
            );
            RequestContext::from_state_with_estimate(
                state,
                retry_nudge.as_ref(),
                Some(cached_message_tokens),
            )
        };
        if request_context.repairs() > 0 {
            tracing::warn!(
                session_id,
                step_num,
                repairs = request_context.repairs(),
                "sanitize_canonical repaired dangling tool calls before LLM"
            );
        }

        let catalog = match before_step.tool_catalog {
            Some(catalog) => catalog,
            None => self.build_tool_catalog_for_session(session_id).await,
        };
        let tools = match before_step.tool_definitions {
            Some(tools) => tools,
            None => Arc::new(
                catalog
                    .provider_definitions()
                    .iter()
                    .cloned()
                    .map(Into::into)
                    .collect(),
            ),
        };
        let router = self.router();
        let request = choose_agent_request(&router, &request_context).await;
        let (request_context, media_plan) = request_context.with_capabilities(
            &router.capability_profile_for_request(request),
            self.media_strategy(),
        );
        // The media plan is a request-preparation signal, not a deferred
        // turn-end projection. Publish it before streaming so cancellation
        // and provider errors cannot drop or reorder it.
        super::emit_media_plan(
            &ctx.emitter,
            session_id,
            step_num,
            ctx.run_id,
            request,
            media_plan,
        )
        .await;
        let mut effects = EffectBatch::continue_batch();
        let partial_thought = Arc::new(std::sync::Mutex::new(String::new()));
        let partial_reasoning = Arc::new(std::sync::Mutex::new(String::new()));

        tracing::debug!(
            "ReAct turn: session={} step={} messages={} tools={}",
            session_id,
            step_num,
            request_context.messages().len(),
            tools.len()
        );
        let mut stream = super::stream_step::StreamSession::new(
            self,
            &ctx,
            router,
            request,
            tools.as_slice(),
            state.identity_map.clone(),
            cancel.clone(),
            &partial_thought,
            &partial_reasoning,
        );
        deadline.ensure_remaining("provider request")?;
        let response = {
            let _timer =
                self.metrics
                    .start(MetricsPhase::LlmStream, session_id, ctx.run_id, step_num);
            match stream
                .run(state, &request_context, retry_nudge.as_ref())
                .instrument(tracing::info_span!("llm", session_id, step_num))
                .await
            {
                StepCallOutcome::Response(response) => *response,
                StepCallOutcome::Cancelled => {
                    if deadline.is_expired() {
                        return Err(anyhow::anyhow!(
                            "turn deadline exceeded during provider request"
                        ));
                    }
                    return Ok(effects.with_exit(LoopExit::Cancelled));
                }
                StepCallOutcome::Fatal(message) => {
                    // `StreamSession` has already persisted the provider error
                    // and any partial scratch output. Keep this as a soft exit so
                    // the outer loop does not overwrite that recovery event
                    // boundary with a generic turn-error path.
                    return Ok(EffectBatch::done(LoopExit::Error(message)));
                }
            }
        };
        deadline.ensure_remaining("provider response")?;

        // Cancellation wins over a late provider response. This prevents a
        // rollback/end-session response from becoming a ghost transcript.
        if cancel.is_cancelled() {
            tracing::info!(
                "ReAct turn cancelled while the model response was in flight: session={} step={}",
                session_id,
                step_num
            );
            return Ok(effects.with_exit(LoopExit::Cancelled));
        }

        // Response-policy retries are isolated from transcript projection. A
        // failed/empty candidate is only visible as streamed scratch output;
        // the accepted response below is the first response that may become
        // durable assistant state.
        let (thought, actions) = Self::parse_default_model_response(&response, step_num);
        deadline.ensure_remaining("response parsing")?;
        let limits = self.limits();
        let pending_ask = !self
            .executor
            .pending_interactions(session_id, crate::interaction::InteractionKind::Ask)
            .await
            .is_empty();
        let AcceptedResponse {
            response,
            thought,
            mut actions,
            empty_retries_remaining,
        } = match self
            .resolve_response_cycle(
                &ctx,
                state,
                &mut stream,
                &request_context,
                response,
                thought,
                actions,
                &cancel,
                cut_off_retries,
                pending_ask,
            )
            .await
        {
            ResponseCycleOutcome::Accepted(accepted) => *accepted,
            ResponseCycleOutcome::Cancelled => {
                if deadline.is_expired() {
                    return Err(anyhow::anyhow!(
                        "turn deadline exceeded during response retry"
                    ));
                }
                return Ok(effects.with_exit(LoopExit::Cancelled));
            }
            ResponseCycleOutcome::RetryableError(message) => {
                // The response-policy failure already persisted the clean
                // pre-response event boundary. Publish the session error from
                // the batch so this soft exit cannot race a second generic
                // failure path or overwrite that recovery marker.
                return Ok(effects.fail_session(message, false));
            }
        };

        if let Some(reasoning) = response.reasoning.clone() {
            let reasoning_id = state.block_msg_id(step_num, ctx.run_id, "reasoning");
            effects.transcript(TranscriptEvent::Reasoning {
                text: reasoning.clone(),
                message_id: reasoning_id.clone(),
            });
            // Reconcile streamed reasoning with the final accepted response.
            effects.push(crate::react::effects::TurnEffect::Emit(
                crate::event::AgentEvent::ReasoningChunk {
                    session_id: session_id.clone(),
                    delta: reasoning,
                    step_number: step_num,
                    run_id: ctx.run_id,
                    message_id: reasoning_id,
                },
            ));
        }

        // An unresolved ask owns the turn. Do not let a synthetic final answer
        // accidentally close it after response retries.
        if pending_ask
            && !actions.is_empty()
            && actions
                .iter()
                .all(|action| action.is_final && action.tool_call_id.is_none())
        {
            actions.clear();
        }

        for event in Self::web_search_return_effects(
            session_id,
            step_num,
            ctx.run_id,
            &response.web_search_calls,
        ) {
            effects.push(crate::react::effects::TurnEffect::Emit(event));
        }

        if let Some(text) = thought.clone() {
            let message_id = state.block_msg_id(step_num, ctx.run_id, "thought");
            effects.transcript(TranscriptEvent::Thought { text, message_id });
        }

        let search_pushed = match Self::prepare_search_context(
            &ctx,
            state,
            &response,
            &thought,
            &actions,
            &mut effects,
        )? {
            SearchContextOutcome::ContinueWithoutTools => return Ok(effects),
            SearchContextOutcome::Proceed {
                assistant_already_pushed,
            } => assistant_already_pushed,
        };

        let thought_projected = thought.is_some()
            || state.events.iter().any(|event| {
                matches!(
                    event,
                    crate::types::TranscriptRecord::Thought { step_number, .. }
                        if *step_number == step_num
                )
            });

        if actions.is_empty() {
            if pending_ask {
                let pending = self
                    .executor
                    .pending_interactions(session_id, crate::interaction::InteractionKind::Ask)
                    .await
                    .into_iter()
                    .next()
                    .ok_or_else(|| anyhow::anyhow!("ask state changed before resume"))?;
                let question = pending.prompt.clone();
                effects.push(crate::react::effects::TurnEffect::ProjectChatMessage {
                    role: "assistant".into(),
                    content: question.clone(),
                    message_type: Some("text".into()),
                    tool_call_id: None,
                    message_id: None,
                });
                effects.pause(
                    step_num + 1,
                    SessionStatus::Paused,
                    Some(haven_common::SessionWaitingReason::Ask),
                    question,
                    None,
                    PauseReason::Ask,
                );
                effects = EffectBatch::with_effects(
                    super::effects::TurnControl::Done(LoopExit::Paused {
                        reason: PauseReason::Ask,
                    }),
                    effects.into_effects(),
                );
                return Ok(effects);
            }
            if thought.is_none() && empty_retries_remaining < limits.empty_response_max_retries {
                let message = "模型连续多次返回空响应（服务端异常）。请稍后点击「继续任务」重试，或检查模型服务状态。";
                // Commit any accepted reasoning/thought effects first, then
                // fail closed. Returning early here used to drop that batch.
                return Ok(effects.fail_session(message, true));
            }
            let text = thought.unwrap_or_else(|| "No action decided.".into());
            let mut end = self
                .finish_turn_end(TurnEndInput {
                    ctx: &ctx,
                    state,
                    final_text: &text,
                    reasoning: response.reasoning.clone(),
                    thinking_blocks: response.thinking_blocks.clone(),
                    already_pushed: search_pushed,
                    thought_projected,
                })
                .await;
            if let Ok(ref mut end) = end {
                end.prepend(effects);
            }
            return end;
        }

        let has_non_final = actions.iter().any(|action| !action.is_final);
        if !has_non_final && actions.iter().any(|action| action.is_final) {
            let text = thought.unwrap_or_else(|| "Session completed.".into());
            let mut end = self
                .finish_turn_end(TurnEndInput {
                    ctx: &ctx,
                    state,
                    final_text: &text,
                    reasoning: response.reasoning.clone(),
                    thinking_blocks: response.thinking_blocks.clone(),
                    already_pushed: search_pushed,
                    thought_projected,
                })
                .await;
            if let Ok(ref mut end) = end {
                end.prepend(effects);
            }
            return end;
        }

        effects.push(crate::react::effects::TurnEffect::ExecuteToolBatch {
            actions,
            thought,
            response,
            catalog,
            cancel,
            allow_tool_retry,
        });
        Ok(effects)
    }
}
