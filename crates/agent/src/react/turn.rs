//! One model sampling turn in the ReAct runtime.
//!
//! A [`Turn`] is deliberately smaller than a run: it prepares context, calls
//! the model once (including response-policy retries), materializes the model
//! response, and either executes one tool batch or reaches a turn boundary.
//! The outer run owns the step budget and lifecycle transitions.

use super::retries::{AfterLlmAction, ResponsePolicyState};
use super::stream_step::SearchContextOutcome;
use super::tool_batch::ToolBatchOutcome;
use super::turn_end::{TurnEndInput, TurnEndOutcome};
use super::*;
use crate::types::{BranchPoint, TranscriptRecord};
use haven_common::types::{CanonicalMessage, CanonicalRole, ContentPart};
use std::collections::HashMap;
use std::sync::Arc;
use tracing::Instrument;

/// Inputs owned by the outer run and borrowed by one model turn.
pub(super) struct TurnInput<'a> {
    pub(super) ctx: StepCtx,
    pub(super) canonical: &'a mut Vec<CanonicalMessage>,
    pub(super) events: &'a mut Vec<TranscriptRecord>,
    pub(super) branch_points: &'a mut HashMap<u32, BranchPoint>,
    pub(super) cancel: tokio_util::sync::CancellationToken,
    pub(super) max_steps: u32,
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
            canonical,
            events,
            branch_points,
            cancel,
            max_steps,
            cut_off_retries,
        } = input;
        let session_id = &ctx.session_id;
        let step_num = ctx.step_num;

        // Context is collected once at the turn boundary and projected by the
        // single transcript writer. Hooks may compact or refresh the context,
        // but they never own queue reads or persistence.
        self.inject_pending_context(&ctx, events, canonical)
            .instrument(tracing::info_span!("inject", session_id, step_num))
            .await;

        let events_before_hooks = events.len();
        self.hooks
            .before_step(self, &ctx, events, canonical)
            .instrument(tracing::info_span!("before_step", session_id, step_num))
            .await;
        // CompactSummary replaces the event log with a new root. Branch
        // cursors into the discarded prefix are no longer meaningful.
        if events.len() < events_before_hooks
            || (events.len() == 1
                && matches!(
                    events.first(),
                    Some(TranscriptRecord::CompactSummary { .. })
                )
                && events_before_hooks > 1)
        {
            branch_points.clear();
        }

        let has_image = canonical_has_image(canonical);
        let repairs = crate::sanitize_canonical(canonical);
        if repairs > 0 {
            tracing::warn!(
                session_id,
                step_num,
                repairs,
                "sanitize_canonical repaired dangling tool calls before LLM"
            );
        }

        let tools = self.build_tool_definitions_for_session(session_id).await;
        let router = self.router();
        let role = choose_agent_role(&router, has_image).await;
        let partial_thought = Arc::new(std::sync::Mutex::new(String::new()));
        let partial_reasoning = Arc::new(std::sync::Mutex::new(String::new()));

        tracing::debug!(
            "ReAct turn: session={} step={} messages={} tools={}",
            session_id,
            step_num,
            canonical.len(),
            tools.len()
        );
        let stream = super::stream_step::StreamSession::new(
            self,
            &ctx,
            router,
            role,
            tools.as_slice(),
            cancel.clone(),
            &partial_thought,
            &partial_reasoning,
        );
        let mut response = match stream
            .run(canonical, events, branch_points)
            .instrument(tracing::info_span!("llm", session_id, step_num))
            .await
        {
            StepCallOutcome::Response(response) => *response,
            StepCallOutcome::Cancelled => {
                return Ok(TurnOutcome::Done(
                    self.exit_cancelled(session_id, events, step_num, branch_points)
                        .await,
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
                self.exit_cancelled(session_id, events, step_num, branch_points)
                    .await,
            ));
        }

        if let Some(reasoning) = response.reasoning.clone() {
            let reasoning_id = self.block_msg_id(session_id, step_num, ctx.run_id, "reasoning");
            self.apply_transcript(
                &ctx,
                TranscriptEvent::Reasoning {
                    text: reasoning.clone(),
                    message_id: reasoning_id.clone(),
                },
                events,
                canonical,
            )
            .await;
            // Reconcile streamed reasoning with the authoritative final text.
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

        let (mut thought, mut actions) = Self::parse_default_model_response(&response, step_num);
        let limits = self.limits();
        let mut empty_retries_remaining = limits.empty_response_max_retries;
        let pending_ask = self
            .executor
            .get_awaiting_answer(session_id)
            .await
            .is_some()
            || Self::canonical_has_pending_ask(canonical);

        // Response policy is an explicit sub-loop. It can request another
        // model call, but it cannot mutate the transcript or decide lifecycle.
        loop {
            let action = self
                .hooks
                .after_llm(
                    self,
                    &ctx,
                    super::hooks::AfterLlmInput {
                        thought: &thought,
                        actions: &actions,
                        response: &response,
                        canonical,
                        state: ResponsePolicyState {
                            empty_retries_remaining,
                            empty_retry_delay_ms: limits.empty_response_retry_delay_ms,
                            cut_off_retries_used: *cut_off_retries,
                            cut_off_retries_max: limits.cut_off_retries,
                            pending_ask,
                        },
                    },
                )
                .await;
            match action {
                AfterLlmAction::Accept => break,
                AfterLlmAction::RetryEmpty { delay_ms } => {
                    empty_retries_remaining = empty_retries_remaining.saturating_sub(1);
                    tokio::select! {
                        biased;
                        _ = cancel.cancelled() => {
                            return Ok(TurnOutcome::Done(
                                self.exit_cancelled(session_id, events, step_num, branch_points).await,
                            ));
                        }
                        _ = tokio::time::sleep(std::time::Duration::from_millis(delay_ms)) => {}
                    }
                    let retry = stream.retry(canonical).await;
                    match retry {
                        Ok(retry_response) => {
                            let (retry_thought, retry_actions) =
                                Self::parse_default_model_response(&retry_response, step_num);
                            if retry_thought.is_some() || !retry_actions.is_empty() {
                                thought = retry_thought;
                                actions = retry_actions;
                                response = retry_response;
                            }
                        }
                        Err(haven_llm::LlmError::Cancelled) => {
                            return Ok(TurnOutcome::Done(
                                self.exit_cancelled(session_id, events, step_num, branch_points)
                                    .await,
                            ));
                        }
                        Err(error) => {
                            tracing::warn!(
                                "ReAct empty-response retry failed: session={} step={} error={}",
                                session_id,
                                step_num,
                                error
                            );
                        }
                    }
                }
                AfterLlmAction::RetryCutOff { nudge } => {
                    *cut_off_retries += 1;
                    let mut retry_messages = canonical.clone();
                    retry_messages.push(CanonicalMessage {
                        role: CanonicalRole::User,
                        content: vec![ContentPart::text(nudge)],
                        tool_call_id: None,
                        tool_calls: None,
                        reasoning: None,
                        web_search_calls: Vec::new(),
                        thinking_blocks: Vec::new(),
                        source: None,
                        id: None,
                    });
                    match stream.retry(&retry_messages).await {
                        Ok(retry_response) => {
                            let (retry_thought, retry_actions) =
                                Self::parse_default_model_response(&retry_response, step_num);
                            if retry_thought.is_some() || !retry_actions.is_empty() {
                                thought = retry_thought;
                                actions = retry_actions;
                                response = retry_response;
                            } else {
                                break;
                            }
                        }
                        Err(haven_llm::LlmError::Cancelled) => {
                            return Ok(TurnOutcome::Done(
                                self.exit_cancelled(session_id, events, step_num, branch_points)
                                    .await,
                            ));
                        }
                        Err(error) => {
                            tracing::warn!(
                                "ReAct cut-off retry failed: session={} step={} error={}",
                                session_id,
                                step_num,
                                error
                            );
                            break;
                        }
                    }
                }
            }
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
            self.apply_transcript(
                &ctx,
                TranscriptEvent::Thought { text, message_id },
                events,
                canonical,
            )
            .await;
        }

        let search_pushed = match self
            .prepare_search_context(
                &ctx,
                &response,
                &thought,
                &actions,
                canonical,
                events,
                branch_points,
            )
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
                    .unwrap_or_else(|| Self::extract_pending_ask_question(canonical));
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
                self.pause_turn(
                    session_id,
                    events,
                    step_num + 1,
                    branch_points,
                    &ctx.emitter,
                    SessionStatus::PausedAwaitingAnswer,
                    &question,
                    None,
                    None,
                    true,
                )
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
                    events,
                    canonical,
                    branch_points,
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
                    events,
                    canonical,
                    branch_points,
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
                canonical,
                events,
                step_num,
                branch_points,
                &ctx.emitter,
                ctx.run_id,
                &mut actions,
                &thought,
                &response,
                &cancel,
                max_steps,
            )
            .instrument(tracing::info_span!("tools", session_id, step_num))
            .await?
        {
            ToolBatchOutcome::Continue => Ok(TurnOutcome::Continue),
            ToolBatchOutcome::Done(exit) => Ok(TurnOutcome::Done(exit)),
        }
    }
}
