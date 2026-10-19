//! One model sampling turn in the ReAct runtime.
//!
//! A [`Turn`] is deliberately smaller than a run: it prepares context, calls
//! the model once (including response-policy retries), materializes the model
//! response, and either executes one tool batch or reaches a turn boundary.
//! The outer run owns the step budget and lifecycle transitions.

use super::effects::{EffectBatch, TurnEffect};
use super::identity::StreamBlockIdentity;
use super::response_cycle::{AcceptedResponse, ResponseCycleOutcome};
use super::tool_batch_policy::ToolRetryBudget;
use super::turn_end::TurnEndInput;
use super::*;
use std::sync::Arc;
use tracing::Instrument;

/// Outcome after provider-side search results have been projected for this turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SearchContextOutcome {
    /// Search round with no answer yet; the caller saves a branch point and continues.
    ContinueWithoutTools,
    /// Proceed to tool execution or turn end. True means the response already
    /// committed its synthesized final answer with the search context.
    Proceed { assistant_already_pushed: bool },
}

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
    pub(super) incomplete_tool_args_retries: &'a mut u32,
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
        step_number: u32,
        run_id: u64,
        web_search_calls: &[serde_json::Value],
    ) -> Vec<crate::event::AgentEvent> {
        let mut events = Vec::new();
        for (item_index, item) in web_search_calls.iter().enumerate() {
            let Some(result) = haven_llm::web_search_result_of(item) else {
                continue;
            };
            let call_id = item
                .get("id")
                .and_then(serde_json::Value::as_str)
                .filter(|call_id| !call_id.is_empty())
                .map(str::to_string);
            let call_index = if call_id.is_none() {
                let Ok(call_index) = u32::try_from(item_index) else {
                    tracing::warn!(item_index, "web-search item index exceeds u32");
                    continue;
                };
                Some(call_index)
            } else {
                None
            };
            events.push(crate::event::AgentEvent::WebSearch {
                session_id: session_id.to_string(),
                phase: haven_llm::WebSearchPhase::Completed,
                step_number,
                run_id,
                call_id,
                call_index,
                action: item
                    .pointer("/action/type")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string),
                result: Some(result),
            });
        }
        events
    }

    /// Project provider server-side search context into the canonical turn
    /// response when no real tool call will carry the search items. Mixed
    /// tool+search responses are handled by `execute_tool_batch` instead.
    #[allow(clippy::too_many_arguments)]
    fn prepare_search_context(
        ctx: &StepCtx,
        state: &ReActState,
        response: &LlmResponse,
        thought: &Option<String>,
        tool_calls: &[AgentToolCall],
        effects: &mut EffectBatch,
    ) -> anyhow::Result<SearchContextOutcome> {
        if response.web_search_calls.is_empty() {
            return Ok(SearchContextOutcome::Proceed {
                assistant_already_pushed: false,
            });
        }
        let synthesized_final = !tool_calls.is_empty()
            && tool_calls
                .iter()
                .all(|tool_call| tool_call.is_final && tool_call.tool_call_id.is_none());
        if !(tool_calls.is_empty() || synthesized_final) {
            // Mixed real tools + search: tool_batch pushes the search items.
            return Ok(SearchContextOutcome::Proceed {
                assistant_already_pushed: false,
            });
        }

        // Text matches Thought projection (trimmed). X12: apply ToolCall so
        // events + canonical stay on the single writer path.
        let push_text = if synthesized_final {
            thought.as_deref().unwrap_or("Session completed.")
        } else {
            thought.as_deref().unwrap_or(&response.text)
        };
        let reasoning = if response.thinking_blocks.is_empty() {
            response.reasoning.clone()
        } else {
            None
        };
        // Thought already projected the messages row when present. When a
        // synthesized final has no Thought, make this owning commit project
        // the same synthetic final text turn-end would otherwise materialize.
        effects.transcript_write(TranscriptWrite::ToolCall {
            text: push_text.to_string(),
            tool_calls: Vec::new(),
            reasoning,
            web_search_calls: response.web_search_calls.clone(),
            thinking_blocks: response.thinking_blocks.clone(),
            tool_call_cards: Vec::new(),
            persist_text_id: (synthesized_final && thought.is_none()).then(|| {
                state.stream_block_message_id_or_new(StreamBlockIdentity::thought(
                    ctx.step_number,
                    ctx.run_id,
                ))
            }),
        });

        if tool_calls.is_empty() {
            // Search round: no answer yet — keep the turn open and re-request
            // with the search context in the next input.
            effects.push(TurnEffect::SaveBranchPoint {
                step_number: ctx.step_number,
            });
            tracing::debug!(
                "ReAct step {} session {} server-side search round ({} item(s)); continuing",
                ctx.step_number,
                ctx.session_id,
                response.web_search_calls.len()
            );
            return Ok(SearchContextOutcome::ContinueWithoutTools);
        }

        // synthesized_final: answer arrived with the search call — fall
        // through to turn-end; the push above keeps search context alive.
        Ok(SearchContextOutcome::Proceed {
            assistant_already_pushed: true,
        })
    }

    async fn run_turn_impl(&self, input: TurnInput<'_>) -> anyhow::Result<EffectBatch> {
        let TurnInput {
            ctx,
            state,
            cancel,
            deadline,
            allow_tool_retry,
            tool_retry_budget: _tool_retry_budget,
            incomplete_tool_args_retries,
        } = input;
        let session_id = &ctx.session_id;
        let step_number = ctx.step_number;
        deadline.ensure_remaining("turn start")?;
        self.metrics.increment(MetricsCounter::TurnStarts);

        // The turn boundary owns context collection, including the sole inbox
        // poll. A separate bounded steering poll follows slow preparation.
        {
            let _timer = self.metrics.start(
                MetricsPhase::ContextInject,
                session_id,
                ctx.run_id,
                step_number,
            );
            self.inject_turn_start_context(&ctx, state)
                .instrument(tracing::info_span!("inject", session_id, step_number))
                .await?;
            deadline.ensure_remaining("context injection")?;
        }

        let before_step = self
            .hooks
            .before_step(self, &ctx, state, cancel.clone())
            .instrument(tracing::info_span!("before_step", session_id, step_number))
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
                step_number,
            );
            state.estimate_canonical_tokens()
        };
        let request_context = {
            let _timer = self.metrics.start(
                MetricsPhase::RequestContext,
                session_id,
                ctx.run_id,
                step_number,
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
                step_number,
                repairs = request_context.repairs(),
                "sanitize_canonical repaired dangling tool calls before LLM"
            );
        }

        let catalog = match before_step.tool_catalog {
            Some(catalog) => catalog,
            None => self.build_tool_catalog_for_session(session_id).await,
        };
        let prepared_tools = match before_step.tool_definitions {
            Some(definitions) => super::sidecars::PreparedToolDefinitions {
                token_estimate: before_step
                    .tool_token_estimate
                    .unwrap_or_else(|| crate::token_budget::estimate_tool_tokens(&definitions)),
                definitions,
            },
            None => self.prepare_tool_definitions(session_id, &catalog),
        };
        let tools = prepared_tools.definitions;
        let tool_token_estimate = prepared_tools.token_estimate;
        let router = self.router();
        let mut request = choose_agent_request(&router, &request_context).await;
        let mut projection = request_context.project_for_capabilities(
            &router.capability_profile_for_request(request),
            self.media_strategy(),
        );
        deadline.ensure_remaining("request preparation")?;
        if cancel.is_cancelled() {
            return Ok(EffectBatch::exit(LoopExit::Cancelled));
        }
        // Exactly one bounded poll: steering received during compaction or
        // catalog/routing preparation should reach this provider request.
        // Arrivals during the rebuild wait for the next normal boundary.
        if self.inject_before_request_steering(&ctx, state).await? {
            let requirements = state.media_requirements();
            self.maybe_compact(
                &ctx,
                state,
                requirements,
                tool_token_estimate,
                cancel.clone(),
            )
            .await?;
            deadline.ensure_remaining("late steering preparation")?;
            if cancel.is_cancelled() {
                return Ok(EffectBatch::exit(LoopExit::Cancelled));
            }
            let cached_message_tokens = state.estimate_canonical_tokens();
            let refreshed = RequestContext::from_state_with_estimate(
                state,
                retry_nudge.as_ref(),
                Some(cached_message_tokens),
            );
            request = choose_agent_request(&router, &refreshed).await;
            projection = refreshed.project_for_capabilities(
                &router.capability_profile_for_request(request),
                self.media_strategy(),
            );
        }
        // The media plan is a request-preparation signal, not a deferred
        // turn-end projection. Publish it before streaming so cancellation
        // and provider errors cannot drop or reorder it.
        super::emit_media_plan(
            &ctx.emitter,
            session_id,
            step_number,
            ctx.run_id,
            request,
            projection.media_plan,
        )
        .await;
        let mut effects = EffectBatch::continue_batch();
        let partial_thought = Arc::new(std::sync::Mutex::new(String::new()));
        let partial_reasoning = Arc::new(std::sync::Mutex::new(String::new()));

        tracing::debug!(
            "ReAct turn: session={} step={} messages={} tools={}",
            session_id,
            step_number,
            projection.request_context.messages().len(),
            tools.len()
        );
        let mut stream = super::stream_step::StreamSession::new(
            self,
            &ctx,
            router,
            request,
            tools.as_slice(),
            tool_token_estimate,
            state.identity_map.clone(),
            cancel.clone(),
            &partial_thought,
            &partial_reasoning,
        );
        deadline.ensure_remaining("provider request")?;
        let streamed_call = {
            let _timer =
                self.metrics
                    .start(MetricsPhase::LlmStream, session_id, ctx.run_id, step_number);
            match stream
                .run(state, &projection.request_context, retry_nudge.as_ref())
                .instrument(tracing::info_span!("llm", session_id, step_number))
                .await
            {
                StepCallOutcome::Response(streamed_call) => *streamed_call,
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
                    return Ok(EffectBatch::exit(LoopExit::Error(message)));
                }
            }
        };
        let super::stream_step::StreamedLlmCall {
            response,
            request_context: provider_request_context,
            ..
        } = streamed_call;
        deadline.ensure_remaining("provider response")?;

        // Cancellation wins over a late provider response. This prevents a
        // rollback/end-session response from becoming a ghost transcript.
        if cancel.is_cancelled() {
            tracing::info!(
                "ReAct turn cancelled while the model response was in flight: session={} step={}",
                session_id,
                step_number
            );
            return Ok(effects.with_exit(LoopExit::Cancelled));
        }

        // Response-policy retries are isolated from transcript projection. A
        // failed/empty candidate is only visible as streamed scratch output;
        // the accepted response below is the first response that may become
        // durable assistant state.
        let parsed_response = Self::parse_default_model_response(&response, step_number);
        let thought = parsed_response.thought;
        let tool_calls = parsed_response.tool_calls;
        deadline.ensure_remaining("response parsing")?;
        let pending_ask = !self
            .executor
            .pending_interactions(session_id, crate::interaction::InteractionKind::Ask)
            .await
            .is_empty();
        let AcceptedResponse {
            response,
            thought,
            mut tool_calls,
        } = match self
            .resolve_response_cycle(
                &ctx,
                state,
                &mut stream,
                &provider_request_context,
                response,
                thought,
                tool_calls,
                &cancel,
                incomplete_tool_args_retries,
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
            ResponseCycleOutcome::RecoverableError(message) => {
                // The response-policy failure already persisted the clean
                // pre-response event boundary. Publish the session error from
                // the batch so this soft exit cannot race a second generic
                // failure path or overwrite that recovery marker.
                return Ok(effects.fail_session(message, false));
            }
        };
        // Release the immutable request Arc before transcript projection. The
        // next canonical append can then mutate the run vector in place
        // instead of triggering an avoidable COW copy of the full history.
        drop(request_context);
        drop(projection.request_context);
        drop(provider_request_context);

        if let Some(reasoning) = response.reasoning.clone() {
            let reasoning_id = state.stream_block_message_id_or_new(
                StreamBlockIdentity::reasoning(step_number, ctx.run_id),
            );
            effects.transcript_write(TranscriptWrite::Reasoning {
                text: reasoning.clone(),
                message_id: reasoning_id.clone(),
            });
            // Reconcile streamed reasoning with the final accepted response.
            effects.push(crate::react::effects::TurnEffect::Emit(
                crate::event::AgentEvent::ReasoningChunk {
                    session_id: session_id.clone(),
                    delta: reasoning,
                    step_number,
                    run_id: ctx.run_id,
                    message_id: reasoning_id,
                },
            ));
        }

        // An unresolved ask owns the turn. Do not let a synthetic final answer
        // accidentally close it after response retries.
        if pending_ask
            && !tool_calls.is_empty()
            && tool_calls
                .iter()
                .all(|tool_call| tool_call.is_final && tool_call.tool_call_id.is_none())
        {
            tool_calls.clear();
        }

        for event in Self::web_search_return_effects(
            session_id,
            step_number,
            ctx.run_id,
            &response.web_search_calls,
        ) {
            effects.push(crate::react::effects::TurnEffect::Emit(event));
        }

        if let Some(text) = thought.clone() {
            let message_id = state.stream_block_message_id_or_new(StreamBlockIdentity::thought(
                step_number,
                ctx.run_id,
            ));
            effects.transcript_write(TranscriptWrite::Thought { text, message_id });
        }

        let search_pushed = match Self::prepare_search_context(
            &ctx,
            state,
            &response,
            &thought,
            &tool_calls,
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
                        if *step_number == ctx.step_number
                )
            });

        if tool_calls.is_empty() {
            if pending_ask {
                let has_pending_ask = self
                    .executor
                    .pending_interactions(session_id, crate::interaction::InteractionKind::Ask)
                    .await
                    .into_iter()
                    .next()
                    .is_some();
                anyhow::ensure!(has_pending_ask, "ask state changed before resume");
                effects.pause(
                    step_number + 1,
                    SessionStatus::Paused,
                    Some(haven_common::lifecycle::SessionWaitingReason::Ask),
                    None,
                    PauseReason::Ask,
                );
                return Ok(effects);
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

        let has_non_final = tool_calls.iter().any(|tool_call| !tool_call.is_final);
        if !has_non_final && tool_calls.iter().any(|tool_call| tool_call.is_final) {
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

        effects.execute_tool_batch(
            tool_calls,
            thought,
            response,
            catalog,
            cancel,
            allow_tool_retry,
        );
        Ok(effects)
    }
}

#[cfg(test)]
mod search_context_tests {
    use super::*;
    use async_trait::async_trait;
    use serde_json::json;
    use std::collections::HashMap;

    struct NoopEmitter;

    #[async_trait]
    impl AgentEventEmitter for NoopEmitter {
        async fn emit(&self, _event: AgentEvent) {}
    }

    #[test]
    fn synthesized_search_final_is_projected_by_its_tool_call_commit() {
        let ctx = StepCtx {
            session_id: "ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into(),
            step_number: 7,
            run_id: 3,
            emitter: Arc::new(NoopEmitter),
        };
        let state = ReActState::new(Vec::new(), Vec::new(), HashMap::new());
        let response = LlmResponse {
            web_search_calls: vec![json!({"type": "web_search_call", "id": "search-1"})],
            ..Default::default()
        };
        let final_tool_call = AgentToolCall {
            tool_name: "final_answer".into(),
            tool_input: json!({}),
            is_final: true,
            tool_call_id: None,
        };
        let mut effects = EffectBatch::continue_batch();

        let outcome = ReActEngine::prepare_search_context(
            &ctx,
            &state,
            &response,
            &None,
            &[final_tool_call],
            &mut effects,
        )
        .unwrap();

        assert_eq!(
            outcome,
            SearchContextOutcome::Proceed {
                assistant_already_pushed: true
            }
        );
        let queued = effects.into_effects();
        let [
            TurnEffect::TranscriptWrite(TranscriptWrite::ToolCall {
                text,
                web_search_calls,
                persist_text_id,
                ..
            }),
        ] = queued.as_slice()
        else {
            panic!("search final must be represented by one committed ToolCall effect");
        };
        assert_eq!(text.as_str(), "Session completed.");
        assert_eq!(web_search_calls, &response.web_search_calls);
        assert!(
            persist_text_id
                .as_deref()
                .is_some_and(|message_id| message_id.starts_with("step-")),
            "synthetic final must carry its shared thought-step message identity"
        );
    }

    #[test]
    fn completed_search_results_without_provider_ids_receive_distinct_call_indices() {
        let events = ReActEngine::web_search_return_effects(
            "ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            7,
            3,
            &[
                json!({"action": {"type": "search", "queries": ["first"], "citations": [{"title": "A", "url": "https://a.test"}]}}),
                json!({"action": {"type": "search", "queries": ["second"], "citations": [{"title": "B", "url": "https://b.test"}]}}),
            ],
        );

        let [
            AgentEvent::WebSearch {
                call_id: first_call_id,
                call_index: Some(0),
                result: Some(first_result),
                ..
            },
            AgentEvent::WebSearch {
                call_id: second_call_id,
                call_index: Some(1),
                result: Some(second_result),
                ..
            },
        ] = events.as_slice()
        else {
            panic!("each id-less citation result must retain its response position");
        };
        assert_eq!(first_call_id, &None);
        assert_eq!(second_call_id, &None);
        assert_ne!(first_result, second_result);
    }

    #[test]
    fn completed_search_result_uses_nonempty_provider_id_without_call_index() {
        let events = ReActEngine::web_search_return_effects(
            "ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            7,
            3,
            &[
                json!({"id": "provider-search-1", "action": {"type": "search", "queries": ["first"]}}),
            ],
        );

        assert!(matches!(
            events.as_slice(),
            [AgentEvent::WebSearch {
                call_id: Some(call_id),
                call_index: None,
                ..
            }] if call_id == "provider-search-1"
        ));
    }
}
