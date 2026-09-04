//! One model sampling turn in the ReAct runtime.
//!
//! A [`Turn`] is deliberately smaller than a run: it prepares context, calls
//! the model once (including response-policy retries), materializes the model
//! response, and either executes one tool batch or reaches a turn boundary.
//! The outer run owns the step budget and lifecycle transitions.

use super::response_cycle::{AcceptedResponse, ResponseCycleOutcome};
use super::snapshot_io::PauseTurnInput;
use super::stream_step::SearchContextOutcome;
use super::tool_batch::ToolBatchOutcome;
use super::turn_end::{TurnEndInput, TurnEndOutcome};
use super::*;
use std::sync::Arc;
use tracing::Instrument;

/// Inputs owned by the outer run and borrowed by one model turn.
pub(super) struct TurnInput<'a> {
    pub(super) ctx: StepCtx,
    pub(super) state: &'a mut ReActState,
    pub(super) cancel: tokio_util::sync::CancellationToken,
    /// Whether this turn may stage a tool-failure retry for another turn.
    /// Computed by the run driver from the absolute run end.
    pub(super) allow_tool_retry: bool,
    pub(super) cut_off_retries: &'a mut u32,
}

/// Control returned to the outer run after a single turn.
pub(super) enum TurnOutcome {
    /// The run may start another turn at the next step boundary.
    Continue,
    /// The turn reached a terminal or pause boundary.
    Done(LoopExit),
}

impl ReActEngine {
    /// Emit completed provider web-search items after the stream has been
    /// folded. Streaming providers already emitted lifecycle updates; this
    /// final pass attaches the compact result payload and covers providers
    /// whose search calls are only visible in the aggregate response.
    async fn emit_web_search_returns(
        emitter: &Arc<dyn AgentEventEmitter>,
        session_id: &str,
        step_num: u32,
        run_id: u64,
        web_search_calls: &[serde_json::Value],
    ) {
        for item in web_search_calls {
            let Some(result) = haven_llm::web_search_result_of(item) else {
                continue;
            };
            emitter
                .emit(crate::event::AgentEvent::WebSearch {
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
                })
                .await;
        }
    }

    pub(super) async fn run_turn(&self, input: TurnInput<'_>) -> anyhow::Result<TurnOutcome> {
        let TurnInput {
            ctx,
            state,
            cancel,
            allow_tool_retry,
            cut_off_retries,
        } = input;
        let session_id = &ctx.session_id;
        let step_num = ctx.step_num;

        // Context is collected once at the turn boundary and projected by the
        // single transcript writer. Hooks may compact or refresh the context,
        // but they never own queue reads or persistence.
        self.inject_turn_start_context(&ctx, state)
            .instrument(tracing::info_span!("inject", session_id, step_num))
            .await;

        self.hooks
            .before_step(self, &ctx, state, cancel.clone())
            .instrument(tracing::info_span!("before_step", session_id, step_num))
            .await;

        // Build one immutable provider projection. Durable canonical state is
        // never used as a scratch buffer by retries or provider repairs.
        let retry_nudge = state.take_retry_nudge();
        let request_context = RequestContext::from_state(state, retry_nudge.as_ref());
        if request_context.repairs() > 0 {
            tracing::warn!(
                session_id,
                step_num,
                repairs = request_context.repairs(),
                "sanitize_canonical repaired dangling tool calls before LLM"
            );
        }

        let tools = self.build_tool_definitions_for_session(session_id).await;
        let router = self.router();
        let role = choose_agent_role(&router, request_context.has_image()).await;
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
            role,
            tools.as_slice(),
            cancel.clone(),
            &partial_thought,
            &partial_reasoning,
        );
        let response = match stream
            .run(state, &request_context, retry_nudge.as_ref())
            .instrument(tracing::info_span!("llm", session_id, step_num))
            .await
        {
            StepCallOutcome::Response(response) => *response,
            StepCallOutcome::Cancelled => {
                return Ok(TurnOutcome::Done(
                    self.exit_cancelled(session_id, state, step_num).await,
                ));
            }
            StepCallOutcome::Fatal(message) => return Err(anyhow::anyhow!(message)),
        };

        // Cancellation wins over a late provider response. This prevents a
        // rollback/end-session response from becoming a ghost transcript.
        if cancel.is_cancelled() {
            tracing::info!(
                "ReAct turn cancelled while the model response was in flight: session={} step={}",
                session_id,
                step_num
            );
            return Ok(TurnOutcome::Done(
                self.exit_cancelled(session_id, state, step_num).await,
            ));
        }

        // Response-policy retries are isolated from transcript projection. A
        // failed/empty candidate is only visible as streamed scratch output;
        // the accepted response below is the first response that may become
        // durable assistant state.
        let request_context = RequestContext::from_state(state, retry_nudge.as_ref());
        let (thought, actions) = Self::parse_default_model_response(&response, step_num);
        let limits = self.limits();
        let pending_ask = self
            .executor
            .get_awaiting_answer(session_id)
            .await
            .is_some()
            || Self::canonical_has_pending_ask(&state.canonical);
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
                return Ok(TurnOutcome::Done(
                    self.exit_cancelled(session_id, state, step_num).await,
                ));
            }
        };

        if let Some(reasoning) = response.reasoning.clone() {
            let reasoning_id = self.block_msg_id(session_id, step_num, ctx.run_id, "reasoning");
            self.apply_transcript(
                &ctx,
                TranscriptEvent::Reasoning {
                    text: reasoning.clone(),
                    message_id: reasoning_id.clone(),
                },
                state,
            )
            .await;
            // Reconcile streamed reasoning with the final accepted response.
            ctx.emitter
                .emit(crate::event::AgentEvent::ReasoningChunk {
                    session_id: session_id.clone(),
                    delta: reasoning,
                    step_number: step_num,
                    run_id: ctx.run_id,
                    message_id: reasoning_id,
                })
                .await;
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

        Self::emit_web_search_returns(
            &ctx.emitter,
            session_id,
            step_num,
            ctx.run_id,
            &response.web_search_calls,
        )
        .await;

        if let Some(text) = thought.clone() {
            let message_id = self.block_msg_id(session_id, step_num, ctx.run_id, "thought");
            self.apply_transcript(&ctx, TranscriptEvent::Thought { text, message_id }, state)
                .await;
        }

        let search_pushed = match self
            .prepare_search_context(&ctx, &response, &thought, &actions, state)
            .await
        {
            SearchContextOutcome::ContinueWithoutTools => return Ok(TurnOutcome::Continue),
            SearchContextOutcome::Proceed {
                assistant_already_pushed,
            } => assistant_already_pushed,
        };

        if actions.is_empty() {
            if pending_ask {
                let pending = self.executor.get_awaiting_answer(session_id).await;
                let question = pending
                    .as_ref()
                    .map(|pending| pending.question.clone())
                    .unwrap_or_else(|| Self::extract_pending_ask_question(&state.canonical));
                if pending.is_none() {
                    self.executor
                        .set_awaiting_answer(
                            session_id,
                            Some(crate::types::AskPending {
                                question: question.clone(),
                                step_ids: Vec::new(),
                            }),
                        )
                        .await;
                }
                self.project_chat_message(
                    session_id,
                    "assistant",
                    &question,
                    Some("text"),
                    None,
                    None,
                )
                .await;
                self.pause_turn(PauseTurnInput {
                    session_id,
                    state,
                    snapshot_step: step_num + 1,
                    emitter: &ctx.emitter,
                    status: SessionStatus::PausedAwaitingAnswer,
                    final_text: &question,
                    branch_point_step: None,
                })
                .await?;
                return Ok(TurnOutcome::Done(LoopExit::Paused {
                    reason: PauseReason::Ask,
                }));
            }
            if thought.is_none() && empty_retries_remaining < limits.empty_response_max_retries {
                let message = "模型连续多次返回空响应（服务端异常）。请稍后点击「继续任务」重试，或检查模型服务状态。";
                self.emit_error(&ctx.emitter, session_id, message).await;
                self.executor
                    .update_session_status(session_id, SessionStatus::Error)
                    .await?;
                return Err(anyhow::anyhow!(message));
            }
            let text = thought.unwrap_or_else(|| "No action decided.".into());
            return self
                .finish_turn_end(TurnEndInput {
                    ctx: &ctx,
                    state,
                    final_text: &text,
                    reasoning: response.reasoning.clone(),
                    thinking_blocks: response.thinking_blocks.clone(),
                    already_pushed: search_pushed,
                })
                .await
                .map(|outcome| match outcome {
                    TurnEndOutcome::Continue => TurnOutcome::Continue,
                    TurnEndOutcome::Done(exit) => TurnOutcome::Done(exit),
                });
        }

        let has_non_final = actions.iter().any(|action| !action.is_final);
        if !has_non_final && actions.iter().any(|action| action.is_final) {
            let text = thought.unwrap_or_else(|| "Session completed.".into());
            return self
                .finish_turn_end(TurnEndInput {
                    ctx: &ctx,
                    state,
                    final_text: &text,
                    reasoning: response.reasoning.clone(),
                    thinking_blocks: response.thinking_blocks.clone(),
                    already_pushed: search_pushed,
                })
                .await
                .map(|outcome| match outcome {
                    TurnEndOutcome::Continue => TurnOutcome::Continue,
                    TurnEndOutcome::Done(exit) => TurnOutcome::Done(exit),
                });
        }

        match self
            .execute_tool_batch(
                session_id,
                state,
                step_num,
                &ctx.emitter,
                ctx.run_id,
                &actions,
                &thought,
                &response,
                &cancel,
                allow_tool_retry,
            )
            .instrument(tracing::info_span!("tools", session_id, step_num))
            .await?
        {
            ToolBatchOutcome::Continue => Ok(TurnOutcome::Continue),
            ToolBatchOutcome::Done(exit) => Ok(TurnOutcome::Done(exit)),
        }
    }
}
