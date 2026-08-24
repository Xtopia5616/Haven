//! Thin ReAct loop driver (`run_react_loop`).
//!
//! Split from `react.rs` (Phase 1 mechanical extract; behavior unchanged).

use super::inject::TurnEndOutcome;
use super::retries::{AfterLlmAction, ResponsePolicyState};
use super::stream_step::SearchContextOutcome;
use super::tool_batch::ToolBatchOutcome;
use super::*;
use crate::types::{BranchPoint, RunBudget, TranscriptRecord};
use haven_common::types::CanonicalMessage;
use std::collections::HashMap;
use std::sync::Arc;
use tracing::Instrument;

impl ReActEngine {
    /// Shared ReAct loop body. Runs from `start_step` through `max_steps`.
    /// Called by both `run_session` (fresh) and `run_session_resumed` (resumed from
    /// snapshot).
    ///
    /// Tool definitions (API `tools[]`) are rebuilt at the top of each step so
    /// tools loaded via `load_skill` / `load_mcp` become visible on the next
    /// step. The system-prompt tools/skills/MCP **index** stays frozen for the
    /// current run (G7 freeze-per-run); resume rebuilds it (X2). Schemas come
    /// from this API list only.
    #[allow(clippy::too_many_arguments)]
    pub async fn run_react_loop(
        &self,
        session_id: &str,
        canonical: &mut Vec<CanonicalMessage>,
        events: &mut Vec<TranscriptRecord>,
        start_step: u32,
        branch_points: &mut HashMap<u32, BranchPoint>,
        emitter: Arc<dyn AgentEventEmitter>,
        run_id: u64,
    ) -> anyhow::Result<LoopExit> {
        let max_steps = *self.max_steps.lock().unwrap();
        let session_cap = *self.session_max_steps.lock().unwrap();
        // Phase 7/8 / J1: per-run budget. Resume grants another full
        // `max_steps` so pause/ask/confirm does not immediately exhaust.
        // Optional `session_max_steps` caps absolute step_number across runs.
        let per_run_cap = max_steps.max(start_step.saturating_sub(1).saturating_add(max_steps));
        let effective_max = match session_cap {
            Some(cap) => per_run_cap.min(cap),
            None => per_run_cap,
        };
        let mut last_step = start_step.saturating_sub(1);
        // One run = one loop invocation: minted streaming-message ids from a
        // previous run of this session are dropped so a fresh run's blocks
        // always get fresh ids (and stale entries never accumulate). The
        // guard clears them again on EVERY exit path (early returns, `?`
        // propagation, cancels), so a session whose last run ends keeps no
        // entries in the engine-wide map. Also clears the R4 run_budget.
        self.clear_msg_ids_for_session(session_id);
        let _msg_id_guard = RunMsgIdGuard {
            engine: self,
            session_id: session_id.to_string(),
        };
        // R4: mirror the effective per-run budget into mid-run / pause snapshots.
        self.set_run_budget(
            session_id,
            RunBudget {
                start_step,
                effective_max,
                max_steps,
                session_max_steps: session_cap,
            },
        );
        tracing::info!(
            "ReAct loop start: session={} run_id={} start_step={} max_steps={} effective_max={}",
            session_id,
            run_id,
            start_step,
            max_steps,
            effective_max
        );
        // Phase 7 / E4: tools may only run while status is Running.
        // Pending→Running + UI emit live in `run_session_from_id` (claim path
        // and direct callers). Never promote from Paused*/terminal here.
        debug_assert_ne!(
            self.executor.get_session_state(session_id).await,
            Some(SessionStatus::Pending),
            "Pending→Running must be completed before run_react_loop"
        );
        // Cut-off retry counter: a text-only response that looks truncated (or
        // is a mid-session narration that stopped without a tool call) is retried
        // up to `context_limits.cut_off_retries` times per run with a continuation nudge (the
        // nudge is not persisted into the canonical). Kept separate from the
        // empty response budget — the two heuristics address different failure
        // modes.
        let mut cut_off_retries: u32 = 0;
        // Phase 2 / C1: pause is exit-based. The loop never parks on
        // `status_rx` — when status is already Paused* at step head we write
        // a snapshot and return `LoopExit::Paused`. Resume is solely the
        // dispatcher reclaiming Pending (F1 single scheduler).

        // Phase 5 / E3: after confirm decisions wake the session, finish the
        // gated tools that were left without results when we paused — before
        // sanitize would repair them as Interrupted.
        if self
            .executor
            .get_awaiting_confirm(session_id)
            .await
            .is_some_and(|p| p.all_decided())
        {
            match self
                .finish_confirm_batch(
                    session_id,
                    canonical,
                    events,
                    branch_points,
                    &emitter,
                    run_id,
                )
                .await?
            {
                ToolBatchOutcome::Continue => {}
                ToolBatchOutcome::Done(exit) => return Ok(exit),
            }
        }

        for step_num in start_step..=effective_max {
            last_step = step_num;
            let cancel = self.executor.cancellation_token(session_id).await;
            // Check cancellation first: end_session / rollback cancel the
            // token, so the loop must exit silently without touching
            // status or emitting events. The state check below would
            // otherwise observe the Error sentinel of a session that
            // end_session already removed from memory and announce a
            // spurious "session interrupted" error. A final snapshot is
            // written so the DB row is never left stale for the rollback
            // that just cancelled us.
            if cancel.is_cancelled() {
                return Ok(self
                    .exit_cancelled(session_id, events, step_num, branch_points)
                    .await);
            }
            let state = self.executor.get_session_state(session_id).await;
            match state {
                // Session vanished from the working set (end_session / terminal
                // cleanup): flush snapshot then exit (aligned with mid-batch).
                None | Some(SessionStatus::Completed) => {
                    return Ok(self
                        .exit_with_snapshot(
                            session_id,
                            events,
                            step_num,
                            branch_points,
                            LoopExit::Completed,
                        )
                        .await);
                }
                Some(SessionStatus::Error) => {
                    // An external path marked the session Error while the
                    // loop was alive: announce the interruption so the
                    // user sees why it stopped.
                    self.emit_error(&emitter, session_id, "session interrupted")
                        .await;
                    return Ok(self
                        .exit_with_snapshot(
                            session_id,
                            events,
                            step_num,
                            branch_points,
                            LoopExit::Error("session interrupted".into()),
                        )
                        .await);
                }
                Some(s) if s.is_paused() => {
                    // External pause (or leftover Paused* after a prior exit
                    // race): snapshot and leave — do not wait for resume here.
                    return Ok(self
                        .exit_external_pause(
                            session_id,
                            events,
                            step_num,
                            branch_points,
                            &emitter,
                            run_id,
                        )
                        .await);
                }
                _ => {}
            }

            // Per-step context shared by all helpers below (context injection,
            // streaming, error handling, final-answer delivery).
            let ctx = StepCtx {
                session_id: session_id.to_string(),
                step_num,
                run_id,
                emitter: emitter.clone(),
            };

            // Deliver user interjections (supplements, steering) and
            // background-action results as context at the top of each step so
            // they land in the gap between tool calls and the next LLM call.
            // Span must not be `.entered()` across `.await` (EnteredSpan is !Send).
            self.inject_pending_context(&ctx, events, canonical)
                .instrument(tracing::info_span!("inject", session_id, step_num))
                .await;

            // Phase 3 / G2 order contract:
            //   inject → hooks.before_step (inbox / compact / interval infer)
            //         → sanitize → tools → LLM
            // The thin loop must not call maybe_poll_inbox / maybe_compact /
            // interval infer directly — those live in DefaultHooks.
            let events_before_hooks = events.len();
            self.hooks
                .before_step(self, &ctx, events, canonical)
                .instrument(tracing::info_span!("before_step", session_id, step_num))
                .await;
            // CompactSummary replace clears the event log to a single root;
            // drop branch points that pointed into the discarded prefix.
            if events.len() < events_before_hooks
                || (events.len() == 1
                    && matches!(
                        events.first(),
                        Some(crate::types::TranscriptRecord::CompactSummary { .. })
                    )
                    && events_before_hooks > 1)
            {
                branch_points.clear();
            }

            // Image flag for endpoint routing: re-scan after hooks (compaction
            // may have summarized away the last image).
            let has_image = canonical_has_image(canonical);

            // No canonical may be sent to the LLM containing a tool message
            // without a preceding assistant tool_calls (providers reject it
            // with a 400). Sanitize as a final gate so compaction or a
            // mid-batch interruption can never poison a request.
            // Phase 7 / J2: count repairs; healthy paths should see 0.
            let repairs = crate::sanitize_canonical(canonical);
            if repairs > 0 {
                // Phase 7 / J2: healthy paths should see 0; non-zero means the
                // gate repaired an upstream interrupt/compaction dangling chain.
                // Interrupt/cancel recovery legitimately repairs — do not
                // `debug_assert_eq!(repairs, 0)` here. Unit tests assert the
                // returned repair count instead.
                tracing::warn!(
                    session_id,
                    step_num,
                    repairs,
                    "sanitize_canonical repaired dangling tool calls before LLM"
                );
            }

            // Rebuild tool definitions each step so that per-session tools
            // registered by `load_skill` / `load_mcp` are visible to the LLM.
            // Cached as Arc — catalog version hits share one schema vec.
            let tools = self.build_tool_definitions_for_session(session_id).await;

            let router = self.router();
            // Same cancellation token as the loop-head wait above; one
            // executor lookup per step instead of two.
            let cancel_res = cancel.clone();
            // Stream from `canonical` directly (no per-step deep clone).
            // Cut-off retries clone only when they append a nudge message.
            let role = choose_agent_role(&router, has_image).await;
            // Accumulate streamed text locally so that if the LLM call fails
            // mid-stream, we can persist whatever was already received instead
            // of losing it entirely.
            let partial_thought: Arc<std::sync::Mutex<String>> =
                Arc::new(std::sync::Mutex::new(String::new()));
            let partial_reasoning: Arc<std::sync::Mutex<String>> =
                Arc::new(std::sync::Mutex::new(String::new()));
            tracing::debug!(
                "ReAct step {} session {} calling LLM, {} messages, {} tools",
                step_num,
                session_id,
                canonical.len(),
                tools.len()
            );
            tracing::trace!(
                "ReAct step {} canonical messages: {:?}",
                step_num,
                canonical
                    .iter()
                    .map(|m| (m.role, m.content.len()))
                    .collect::<Vec<_>>()
            );
            // Phase 5 / E1: stream via StreamSession — loop never builds
            // StreamForwarder; retries reuse the same session (msg-ids).
            let stream = super::stream_step::StreamSession::new(
                self,
                &ctx,
                router.clone(),
                role,
                tools.as_slice(),
                cancel_res.clone(),
                &partial_thought,
                &partial_reasoning,
            );
            let mut response = match stream
                .run(canonical, events, branch_points)
                .instrument(tracing::info_span!("llm", session_id, step_num))
                .await
            {
                StepCallOutcome::Response(resp) => resp,
                StepCallOutcome::Cancelled => {
                    // A final snapshot keeps the DB row current for the
                    // rollback/continue that cancelled the LLM call (the
                    // response was never parsed, so the saved state is the
                    // clean pre-step state).
                    return Ok(self
                        .exit_cancelled(session_id, events, step_num, branch_points)
                        .await);
                }
                StepCallOutcome::Fatal(msg) => return Err(anyhow::anyhow!("{}", msg)),
            };

            // L2/C2: a rollback or end_session may have cancelled the session while
            // the LLM call was in flight (the HTTP call itself may not observe
            // the token promptly and can return well after the 5s rollback
            // wait). Re-check before persisting anything so a stale response
            // cannot overwrite the restored snapshot or push ghost steps.
            if cancel_res.is_cancelled() {
                tracing::info!(
                    "ReAct step {} session {} cancelled during LLM call; discarding response",
                    step_num,
                    session_id
                );
                return Ok(self
                    .exit_cancelled(session_id, events, step_num, branch_points)
                    .await);
            }

            tracing::debug!(
                "ReAct step {} LLM response: {} text chars, {} tool_calls, reasoning={}",
                step_num,
                response.text.len(),
                response.tool_calls.len(),
                response.reasoning.is_some()
            );

            if let Some(ref reasoning) = response.reasoning {
                let reasoning_id = self.block_msg_id(session_id, step_num, run_id, "reasoning");
                let step_ctx = StepCtx {
                    session_id: session_id.to_string(),
                    step_num,
                    run_id,
                    emitter: emitter.clone(),
                };
                // X12: reasoning row is projected from apply (events authority).
                self.apply_transcript(
                    &step_ctx,
                    TranscriptEvent::Reasoning {
                        text: reasoning.clone(),
                        message_id: reasoning_id.clone(),
                    },
                    events,
                    canonical,
                )
                .await;
                // Reconcile the frontend's streamed reasoning with the
                // authoritative complete text. The frontend builds reasoning
                // only from batched deltas, so a dropped/delayed final chunk
                // would permanently lose trailing characters. Emitting the
                // complete reasoning as a final delta lets the frontend's
                // cumulative-detection (delta.startsWith(curr) —replace)
                // snap the content to the exact full text. This runs after the
                // chunk batcher has flushed, so it is guaranteed to be the
                // last reasoning event for this step. The delta carries the
                // same minted message id the streamed chunks used.
                emitter
                    .emit(crate::event::AgentEvent::ReasoningChunk {
                        session_id: session_id.into(),
                        delta: reasoning.clone(),
                        step_number: step_num,
                        run_id,
                        message_id: reasoning_id,
                    })
                    .await;
            }

            let (mut thought, mut actions) =
                Self::parse_default_model_response(&response, step_num);

            // Phase 5 / G3: empty / cut-off classification lives in
            // ResponsePolicy via hooks.after_llm — the thin loop has no
            // phrase literals and only orchestrates retries.
            let limits = self.limits();
            let mut empty_retries_remaining = limits.empty_response_max_retries;
            let pending_ask = self
                .executor
                .get_awaiting_answer(session_id)
                .await
                .is_some()
                || Self::canonical_has_pending_ask(canonical);
            loop {
                let policy_state = ResponsePolicyState {
                    empty_retries_remaining,
                    empty_retry_delay_ms: limits.empty_response_retry_delay_ms,
                    cut_off_retries_used: cut_off_retries,
                    cut_off_retries_max: limits.cut_off_retries,
                    pending_ask,
                };
                let action = self
                    .hooks
                    .after_llm(
                        self,
                        &ctx,
                        &thought,
                        &actions,
                        &response,
                        canonical,
                        policy_state,
                    )
                    .await;
                match action {
                    AfterLlmAction::Accept => break,
                    AfterLlmAction::RetryEmpty { delay_ms } => {
                        empty_retries_remaining =
                            empty_retries_remaining.saturating_sub(1);
                        if cancel_res.is_cancelled() {
                            return Ok(self
                                .exit_cancelled(session_id, events, step_num, branch_points)
                                .await);
                        }
                        tokio::select! {
                            _ = cancel_res.cancelled() => {
                                return Ok(self
                                    .exit_cancelled(session_id, events, step_num, branch_points)
                                    .await);
                            }
                            _ = tokio::time::sleep(std::time::Duration::from_millis(delay_ms)) => {}
                        }
                        if cancel_res.is_cancelled() {
                            return Ok(self
                                .exit_cancelled(session_id, events, step_num, branch_points)
                                .await);
                        }
                        tracing::warn!(
                            "ReAct step {} session {} model returned an empty response; retrying ({} left)",
                            step_num,
                            session_id,
                            empty_retries_remaining
                        );
                        match stream.retry(canonical).await {
                            Ok(retry_resp) => {
                                let (t2, a2) =
                                    Self::parse_default_model_response(&retry_resp, step_num);
                                if t2.is_some() || !a2.is_empty() {
                                    thought = t2;
                                    actions = a2;
                                    response = retry_resp;
                                } else {
                                    tracing::warn!(
                                        "ReAct step {} retry also returned an empty response",
                                        step_num
                                    );
                                }
                            }
                            Err(haven_llm::LlmError::Cancelled) => {
                                return Ok(self
                                    .exit_cancelled(session_id, events, step_num, branch_points)
                                    .await);
                            }
                            Err(e) => {
                                tracing::warn!(
                                    "ReAct step {} empty-response retry failed: {}",
                                    step_num,
                                    e
                                );
                            }
                        }
                    }
                    AfterLlmAction::RetryCutOff { nudge } => {
                        cut_off_retries += 1;
                        if cancel_res.is_cancelled() {
                            return Ok(self
                                .exit_cancelled(session_id, events, step_num, branch_points)
                                .await);
                        }
                        tracing::warn!(
                            "ReAct step {} session {} response looks cut off (finish={:?}); retrying (attempt {}/{})",
                            step_num,
                            session_id,
                            response.finish_reason,
                            cut_off_retries,
                            limits.cut_off_retries
                        );
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
                            Ok(retry_resp) => {
                                let (t2, a2) =
                                    Self::parse_default_model_response(&retry_resp, step_num);
                                if t2.is_some() || !a2.is_empty() {
                                    thought = t2;
                                    actions = a2;
                                    response = retry_resp;
                                } else {
                                    tracing::warn!(
                                        "ReAct step {} cut-off retry also returned an empty response",
                                        step_num
                                    );
                                    break;
                                }
                            }
                            Err(haven_llm::LlmError::Cancelled) => {
                                return Ok(self
                                    .exit_cancelled(session_id, events, step_num, branch_points)
                                    .await);
                            }
                            Err(e) => {
                                tracing::warn!(
                                    "ReAct step {} cut-off retry failed: {}",
                                    step_num,
                                    e
                                );
                                break;
                            }
                        }
                    }
                }
            }

            // An unanswered `ask` still pending in the canonical (a reply was
            // lost to compaction/sanitization, or the model never resolved
            // the question): the text-only-Stop heuristic must not end the
            // turn. Drop the synthesized final so the empty-actions path
            // below re-surfaces the question and pauses for the user's
            // answer instead of "completing" with the question unanswered.
            // Applied after the retries so a retry that produced a
            // synthesized final is covered too; explicit final tool calls
            // (the model decided to answer despite the pending question) are
            // respected.
            if pending_ask
                && !actions.is_empty()
                && actions
                    .iter()
                    .all(|a| a.is_final && a.tool_call_id.is_none())
            {
                tracing::warn!(
                    "ReAct step {} session {} stopped while an ask is pending; keeping the turn open",
                    step_num,
                    session_id
                );
                actions.clear();
            }

            // Phase 7 / E5: schema repair moved into execute_tool_batch.

            tracing::trace!(
                "ReAct step {} parsed: thought={}, actions={}",
                step_num,
                thought
                    .as_ref()
                    .map(|t| format!("{} chars", t.len()))
                    .unwrap_or_else(|| "none".into()),
                actions
                    .iter()
                    .map(|a| a.tool_name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            );

            if let Some(ref t) = thought {
                let message_id = self.block_msg_id(session_id, step_num, run_id, "thought");
                let step_ctx = StepCtx {
                    session_id: session_id.to_string(),
                    step_num,
                    run_id,
                    emitter: emitter.clone(),
                };
                self.apply_transcript(
                    &step_ctx,
                    TranscriptEvent::Thought {
                        text: t.clone(),
                        message_id,
                    },
                    events,
                    canonical,
                )
                .await;
            }

            // Phase 7 / G4: provider search context prepared outside the thin
            // loop (`prepare_search_context` → ContinueWithoutTools / Proceed).
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
                SearchContextOutcome::ContinueWithoutTools => continue,
                SearchContextOutcome::Proceed {
                    assistant_already_pushed,
                } => assistant_already_pushed,
            };

            if actions.is_empty() {
                // An `ask` is still pending unanswered (the model stopped
                // without resolving it, and no user reply arrived to pair
                // with it): the turn must not end on a heuristic final.
                // Re-surface the question and pause so the user's next
                // message is treated as the answer.
                if pending_ask {
                    let pending = self.executor.get_awaiting_answer(session_id).await;
                    let question = pending
                        .as_ref()
                        .map(|p| p.question.clone())
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
                    // Re-surface as a messages-only row (fresh id) so resume
                    // can re-seed chat history without polluting events /
                    // canonical. Review still prefers the original ask card.
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
                        &emitter,
                        SessionStatus::PausedAwaitingAnswer,
                        &question,
                        None,
                        None,
                        true,
                    )
                    .await?;
                    return Ok(LoopExit::Paused {
                        reason: PauseReason::Ask,
                    });
                }
                // The empty-response retries all failed: the model produced
                // nothing (no text, no tool calls) on every attempt. Ending
                // the turn with a fake "No action decided." answer would look
                // like the assistant ignored the user — surface an explicit
                // error instead so the user can retry the session, and the real
                // cause (upstream silent failure) is visible.
                if thought.is_none()
                    && empty_retries_remaining < limits.empty_response_max_retries
                {
                    let err_msg = "模型连续多次返回空响应（服务端异常）。请稍后点击「继续任务」重试，或检查模型服务状态。"
                        .to_string();
                    self.emit_error(&emitter, session_id, &err_msg).await;
                    self.executor
                        .update_session_status(session_id, SessionStatus::Error)
                        .await?;
                    return Err(anyhow::anyhow!("{}", err_msg));
                }
                let msg = thought.unwrap_or_else(|| "No action decided.".into());
                // Phase 7 / C6: shared turn-end (empty actions → TurnEnd).
                match self
                    .finish_turn_end(
                        &ctx,
                        events,
                        canonical,
                        branch_points,
                        &msg,
                        response.reasoning.clone(),
                        response.thinking_blocks.clone(),
                        search_pushed,
                    )
                    .await?
                {
                    TurnEndOutcome::Continue => continue,
                    TurnEndOutcome::Done(exit) => return Ok(exit),
                }
            }

            // Phase 7 / C6: explicit final only ends the turn when there are
            // no non-final tools. A mixed response (tools + final_answer)
            // must run the tool batch first — otherwise non-final calls are
            // dropped and never reach execute_tool_batch.
            let has_non_final = actions.iter().any(|a| !a.is_final);
            if !has_non_final && actions.iter().any(|a| a.is_final) {
                let final_text = thought.unwrap_or_else(|| "Session completed.".into());
                // Search context may already be in the canonical
                // (`prepare_search_context`) — do not duplicate the assistant.
                match self
                    .finish_turn_end(
                        &ctx,
                        events,
                        canonical,
                        branch_points,
                        &final_text,
                        response.reasoning.clone(),
                        response.thinking_blocks.clone(),
                        search_pushed,
                    )
                    .await?
                {
                    TurnEndOutcome::Continue => continue,
                    TurnEndOutcome::Done(exit) => return Ok(exit),
                }
            }

            // Thought text was projected inside apply_transcript(Thought).

            match self
                .execute_tool_batch(
                    session_id,
                    canonical,
                    events,
                    step_num,
                    branch_points,
                    &emitter,
                    run_id,
                    &mut actions,
                    &thought,
                    &response,
                    &cancel_res,
                    max_steps,
                )
                .instrument(tracing::info_span!("tools", session_id, step_num))
                .await?
            {
                ToolBatchOutcome::Continue => {}
                ToolBatchOutcome::Done(exit) => return Ok(exit),
            }
        }

        self.pause_turn_budget(
            session_id,
            events,
            last_step + 1,
            branch_points,
            &emitter,
        )
        .await?;
        Ok(LoopExit::Paused {
            reason: PauseReason::Budget,
        })
    }
}
