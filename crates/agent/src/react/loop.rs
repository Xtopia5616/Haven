//! Thin ReAct loop driver (`run_react_loop`).
//!
//! Split from `react.rs` (Phase 1 mechanical extract; behavior unchanged).

use super::retries::{CUT_OFF_RETRY_NUDGE, MID_ACTION_RETRY_NUDGE};
use super::tool_batch::ToolBatchOutcome;
use super::*;
use crate::types::{BranchPoint, ReActStep};
use haven_common::types::CanonicalMessage;
use std::collections::HashMap;
use std::sync::Arc;

impl ReActEngine {
    /// Shared ReAct loop body. Runs from `start_step` through `max_steps`.
    /// Called by both `run_session` (fresh) and `run_session_resumed` (resumed from
    /// snapshot).
    ///
    /// Tool definitions are rebuilt at the top of each step so that tools
    /// loaded via `load_skill` / `load_mcp` (registered per-session) become
    /// visible to the LLM on the very next step.
    #[allow(clippy::too_many_arguments)]
    pub async fn run_react_loop(
        &self,
        session_id: &str,
        canonical: &mut Vec<CanonicalMessage>,
        history: &mut Vec<ReActStep>,
        start_step: u32,
        branch_points: &mut HashMap<u32, BranchPoint>,
        emitter: Arc<dyn AgentEventEmitter>,
        infer: &(dyn Fn(bool) + Send + Sync),
        run_id: u64,
    ) -> anyhow::Result<LoopExit> {
        let max_steps = *self.max_steps.lock().unwrap();
        // When resuming past the configured cap (e.g. a session that used all
        // `max_steps` then paused for the user's next turn), give the loop
        // another full budget so the resume doesn't degenerate into an
        // immediate budget-exhaustion pause below. This intentionally
        // re-budgets on every resume —a session can run `max_steps` per run,
        // not once per session lifetime (documented in refactor-dedup.md A9).
        let effective_max = max_steps.max(start_step.saturating_sub(1).saturating_add(max_steps));
        let mut last_step = start_step.saturating_sub(1);
        // One run = one loop invocation: minted streaming-message ids from a
        // previous run of this session are dropped so a fresh run's blocks
        // always get fresh ids (and stale entries never accumulate). The
        // guard clears them again on EVERY exit path (early returns, `?`
        // propagation, cancels), so a session whose last run ends keeps no
        // entries in the engine-wide map.
        self.clear_msg_ids_for_session(session_id);
        let _msg_id_guard = RunMsgIdGuard {
            engine: self,
            session_id: session_id.to_string(),
        };
        tracing::info!(
            "ReAct loop start: session={} run_id={} start_step={} max_steps={} effective_max={}",
            session_id,
            run_id,
            start_step,
            max_steps,
            effective_max
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
                self.save_exit_snapshot(
                    session_id,
                    canonical,
                    history,
                    step_num,
                    branch_points,
                )
                .await;
                return Ok(LoopExit::Cancelled);
            }
            let state = self.executor.get_session_state(session_id).await;
            match state {
                // Session vanished from the working set (end_session / terminal
                // cleanup): exit silently.
                None | Some(SessionStatus::Completed) => {
                    return Ok(LoopExit::Completed);
                }
                Some(SessionStatus::Error) => {
                    // An external path marked the session Error while the
                    // loop was alive: announce the interruption so the
                    // user sees why it stopped.
                    self.emit_error(&emitter, session_id, "session interrupted")
                        .await;
                    return Ok(LoopExit::Error("session interrupted".into()));
                }
                Some(s) if s.is_paused() => {
                    // External pause (or leftover Paused* after a prior exit
                    // race): snapshot and leave — do not wait for resume here.
                    self.save_snapshot_with_branches(
                        session_id,
                        canonical,
                        history,
                        step_num,
                        branch_points,
                    )
                    .await;
                    let ctx = StepCtx {
                        session_id: session_id.to_string(),
                        step_num,
                        run_id,
                        emitter: emitter.clone(),
                    };
                    // G2: pause infer goes through on_pause (including External).
                    self.hooks
                        .on_pause(self, &ctx, PauseReason::External, infer)
                        .await;
                    return Ok(LoopExit::Paused {
                        reason: PauseReason::External,
                    });
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
            self.inject_pending_context(&ctx, canonical).await;

            // Phase 3 / G2 order contract:
            //   inject → hooks.before_step (inbox / compact / interval infer)
            //         → sanitize → tools → LLM
            // The thin loop must not call maybe_poll_inbox / maybe_compact /
            // interval infer directly — those live in DefaultHooks.
            self.hooks
                .before_step(self, &ctx, canonical, infer)
                .await;

            // Image flag for endpoint routing: re-scan after hooks (compaction
            // may have summarized away the last image).
            let has_image = canonical_has_image(canonical);

            // No canonical may be sent to the LLM containing a tool message
            // without a preceding assistant tool_calls (providers reject it
            // with a 400). Sanitize as a final gate so compaction or a
            // mid-batch interruption can never poison a request.
            crate::sanitize_canonical(canonical);

            // Rebuild tool definitions each step so that per-session tools
            // registered by `load_skill` / `load_mcp` are visible to the LLM.
            let tools: Vec<ToolDefinition> =
                self.build_tool_definitions_for_session(session_id).await;

            let router = self.router();
            // Same cancellation token as the loop-head wait above; one
            // executor lookup per step instead of two.
            let cancel_res = cancel.clone();
            // Convert once per step; retries below reuse the converted
            // messages (the canonical is only replaced by the compaction
            // path, which re-converts) instead of cloning the whole
            // canonical and re-serializing every tool-call argument again.
            let mut llm_messages = canonical.clone();
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
                llm_messages.len(),
                tools.len()
            );
            tracing::trace!(
                "ReAct step {} canonical messages: {:?}",
                step_num,
                llm_messages
                    .iter()
                    .map(|m| (m.role, m.content.len()))
                    .collect::<Vec<_>>()
            );
            let mut response = match self
                .call_step_llm(
                    &ctx,
                    router.clone(),
                    role,
                    &mut llm_messages,
                    &tools,
                    cancel_res.clone(),
                    canonical,
                    history,
                    branch_points,
                    &partial_thought,
                    &partial_reasoning,
                )
                .await
            {
                StepCallOutcome::Response(resp) => resp,
                StepCallOutcome::Cancelled => {
                    // A final snapshot keeps the DB row current for the
                    // rollback/continue that cancelled the LLM call (the
                    // response was never parsed, so the saved state is the
                    // clean pre-step state).
                    self.save_exit_snapshot(
                        session_id,
                        canonical,
                        history,
                        step_num,
                        branch_points,
                    )
                    .await;
                    return Ok(LoopExit::Cancelled);
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
                self.save_exit_snapshot(session_id, canonical, history, step_num, branch_points)
                    .await;
                return Ok(LoopExit::Cancelled);
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
                self.persist_session_message(
                    session_id,
                    "assistant",
                    reasoning,
                    Some("reasoning"),
                    None,
                    Some(&reasoning_id),
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

            // Empty-response retry budget for THIS step. A completely empty
            // model response (no text, no reasoning, no tool calls) is almost
            // always a transient upstream glitch. Retry the same context up
            // to `context_limits.empty_response_max_retries` times before concluding the model
            // decided nothing — otherwise the session would instantly "complete"
            // with a "No action decided." message and pause without answering.
            // Declared per step so an earlier empty response in this run
            // cannot starve a later incident of its retries; the exhausted
            // state (reached 0) also drives the explicit error path below.
            let mut empty_retries_remaining = self.context_limits.empty_response_max_retries;
            // A response carrying `web_search_call` items is NOT empty: it is
            // a server-side search round that must round-trip instead.
            if thought.is_none() && actions.is_empty() && response.web_search_calls.is_empty() {
                while empty_retries_remaining > 0 {
                    empty_retries_remaining -= 1;
                    if cancel_res.is_cancelled() {
                        self.save_exit_snapshot(
                            session_id,
                            canonical,
                            history,
                            step_num,
                            branch_points,
                        )
                        .await;
                        return Ok(LoopExit::Cancelled);
                    }
                    // Settling delay between attempts: an upstream glitch that
                    // just produced an empty stream often clears within a
                    // second or two. Cancellable: a user rollback / end_session
                    // during the delay must not wait out the whole sleep — the
                    // retry loop is exited as soon as the token fires, so the
                    // handler releases the running slot promptly instead of
                    // blocking the rollback's 5s wait.
                    tokio::select! {
                        _ = cancel_res.cancelled() => {
                            self.save_exit_snapshot(session_id, canonical, history, step_num, branch_points)
                                .await;
                            return Ok(LoopExit::Cancelled);
                        }
                        _ = tokio::time::sleep(std::time::Duration::from_millis(
                            self.context_limits.empty_response_retry_delay_ms,
                        )) => {}
                    }
                    if cancel_res.is_cancelled() {
                        self.save_exit_snapshot(
                            session_id,
                            canonical,
                            history,
                            step_num,
                            branch_points,
                        )
                        .await;
                        return Ok(LoopExit::Cancelled);
                    }
                    tracing::warn!(
                        "ReAct step {} session {} model returned an empty response; retrying ({} left)",
                        step_num,
                        session_id,
                        empty_retries_remaining
                    );
                    // Cancellable: end_session / rollback must be able to abort
                    // the retries mid-flight (the non-cancellable variant
                    // would also grant each attempt a fresh total-duration
                    // budget instead of sharing the step's cancellation).
                    // Chunks are forwarded live so a recovering provider is
                    // visible instead of freezing the UI for the whole budget.
                    match self
                        .stream_retry_step(
                            &ctx,
                            router.clone(),
                            role,
                            &llm_messages,
                            &tools,
                            cancel_res.clone(),
                            &partial_thought,
                            &partial_reasoning,
                        )
                        .await
                    {
                        Ok(retry_resp) => {
                            let (t2, a2) =
                                Self::parse_default_model_response(&retry_resp, step_num);
                            if t2.is_some() || !a2.is_empty() {
                                thought = t2;
                                actions = a2;
                                // The retry produced the content: the whole
                                // response must follow it, or the canonical
                                // assistant message would carry the retry's
                                // tool calls WITHOUT its reasoning (DeepSeek
                                // thinking mode 400s on the next request).
                                response = retry_resp;
                                break;
                            }
                            tracing::warn!(
                                "ReAct step {} retry also returned an empty response",
                                step_num
                            );
                        }
                        Err(haven_llm::LlmError::Cancelled) => {
                            self.save_exit_snapshot(
                                session_id,
                                canonical,
                                history,
                                step_num,
                                branch_points,
                            )
                            .await;
                            return Ok(LoopExit::Cancelled);
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
            }

            // A text-only response that ends without an explicit tool call is
            // only trusted as a deliberate final answer when it looks
            // complete: the provider reported Stop AND the text does not end
            // mid-sentence AND the agent is not still mid-session. Anything else
            // —a truncated generation (Length / ContentFilter / unknown
            // finish), text cut off mid-thought, or a mid-session narration that
            // stopped without issuing the tool call it described —must not
            // end the turn presenting a partial answer as final.
            // Retry with a continuation nudge (never persisted into the
            // canonical), up to `context_limits.cut_off_retries` times; if every retry is
            // also unusable, fall back to the original text below.
            // Prefer the explicit awaiting flag (C5); fall back to the legacy
            // JSON scan only when the flag is absent (older snapshots).
            let pending_ask = self
                .executor
                .get_awaiting_answer(session_id)
                .await
                .is_some()
                || Self::canonical_has_pending_ask(canonical);
            // Responses that carried a web search call must never be re-asked:
            // the search round itself is a legitimate (non-cut-off) outcome,
            // and retrying would trigger a duplicate server-side search.
            while cut_off_retries < self.context_limits.cut_off_retries
                && !pending_ask
                && response.web_search_calls.is_empty()
                && Self::is_suspect_final(&thought, &actions, &response, canonical)
            {
                cut_off_retries += 1;
                if cancel_res.is_cancelled() {
                    self.save_exit_snapshot(
                        session_id,
                        canonical,
                        history,
                        step_num,
                        branch_points,
                    )
                    .await;
                    return Ok(LoopExit::Cancelled);
                }
                // Mid-session narrations get a stronger nudge that explicitly asks
                // for the pending tool call; plain truncation gets the generic
                // continuation nudge.
                let mid_session = Self::canonical_has_pending_tool_context(canonical);
                let nudge = if mid_session {
                    MID_ACTION_RETRY_NUDGE
                } else {
                    CUT_OFF_RETRY_NUDGE
                };
                tracing::warn!(
                    "ReAct step {} session {} response looks cut off (finish={:?}, mid_session={}); retrying (attempt {}/{})",
                    step_num,
                    session_id,
                    response.finish_reason,
                    mid_session,
                    cut_off_retries,
                    self.context_limits.cut_off_retries
                );
                let mut retry_messages = llm_messages.clone();
                retry_messages.push(CanonicalMessage {
                    role: CanonicalRole::User,
                    content: vec![ContentPart::text(nudge)],
                    tool_call_id: None,
                    tool_calls: None,
                    reasoning: None,
                    web_search_calls: Vec::new(),
                    thinking_blocks: Vec::new(),
                });
                // Cancellable so an interruption (rollback / end_session) aborts
                // the retry promptly instead of letting its stream run to
                // completion after the session was cancelled (the empty-response
                // retry below uses the same cancellable variant). Chunks are
                // forwarded live like the primary call, so a resumed provider
                // streams visibly instead of freezing the UI mid-step.
                match self
                    .stream_retry_step(
                        &ctx,
                        router.clone(),
                        role,
                        &retry_messages,
                        &tools,
                        cancel_res.clone(),
                        &partial_thought,
                        &partial_reasoning,
                    )
                    .await
                {
                    Ok(retry_resp) => {
                        let (t2, a2) = Self::parse_default_model_response(&retry_resp, step_num);
                        if t2.is_some() || !a2.is_empty() {
                            thought = t2;
                            actions = a2;
                            // Same reasoning-attachment rule as the
                            // empty-response retry: the canonical push must
                            // carry the retry response's own reasoning, not
                            // the cut-off original's.
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
                        self.save_exit_snapshot(
                            session_id,
                            canonical,
                            history,
                            step_num,
                            branch_points,
                        )
                        .await;
                        return Ok(LoopExit::Cancelled);
                    }
                    Err(e) => {
                        tracing::warn!("ReAct step {} cut-off retry failed: {}", step_num, e);
                        break;
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

            // Repair tool-call arguments that are missing required fields
            // before they reach the provider / tool (common after an
            // interrupted/continued generation). A missing required field is
            // filled from the schema default, else a type placeholder, so the
            // call deserializes instead of triggering a 400. Runs on the
            // finalized actions so retry-replaced responses are covered too.
            if !actions.is_empty() {
                let repaired = self
                    .supplement_missing_required_fields(session_id, &mut actions)
                    .await;
                if repaired > 0 {
                    tracing::warn!(
                        "ReAct step {} session {} repaired {} tool call(s) with missing required fields",
                        step_num,
                        session_id,
                        repaired
                    );
                }
            }

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
                EventDispatcher::emit_thought_from(
                    &emitter,
                    session_id,
                    t,
                    step_num,
                    run_id,
                    &message_id,
                    &self.db,
                )
                .await;
                history.push(ReActStep {
                    step_number: step_num,
                    thought: Some(t.clone()),
                    action: None,
                    observation: None,
                });
            }

            // ── Web search round-trip ─────────────────────────────────────
            // `web_search_call` output items come from the provider's
            // server-side search tool (DeepSeek built-in). The search itself
            // runs on the provider; the items must be passed back verbatim in
            // the next request's input so the server restores the search
            // context. Push an assistant message carrying them into the
            // canonical so every subsequent path round-trips them.
            let has_web_search = !response.web_search_calls.is_empty();
            let synthesized_final = !actions.is_empty()
                && actions
                    .iter()
                    .all(|a| a.is_final && a.tool_call_id.is_none());
            if has_web_search && (actions.is_empty() || synthesized_final) {
                // The text must match what `persist_session_message` stores
                // (trimmed thought) or the resume dedup fails on the leading
                // whitespace and re-seeds the message as a [conversation] line.
                let push_text = thought.as_deref().unwrap_or(&response.text);
                canonical.push(CanonicalMessage::assistant(
                    vec![ContentPart::text(push_text.to_string())],
                    None,
                    if response.thinking_blocks.is_empty() {
                        response.reasoning.clone()
                    } else {
                        None
                    },
                    response.web_search_calls.clone(),
                    response.thinking_blocks.clone(),
                ));
                if actions.is_empty() {
                    // Search round: no answer yet, the follow-up request
                    // carries the search context and produces the answer.
                    // Keep the turn open and let the loop re-request.
                    if let Some(ref t) = thought {
                        let message_id = self.block_msg_id(session_id, step_num, run_id, "thought");
                        self.persist_session_message(
                            session_id,
                            "assistant",
                            t,
                            Some("text"),
                            None,
                            Some(&message_id),
                        )
                        .await;
                    }
                    self.save_branch_point(
                        session_id,
                        canonical,
                        history,
                        step_num,
                        branch_points,
                        false,
                    )
                    .await;
                    tracing::debug!(
                        "ReAct step {} session {} server-side web search round ({} item(s)); continuing",
                        step_num,
                        session_id,
                        response.web_search_calls.len()
                    );
                    continue;
                }
                // synthesized_final: the answer arrived in the same response
                // as the search call —fall through to the final-answer path,
                // which ends the turn. The canonical push above keeps the
                // search context alive for follow-up turns.
            }

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
                    self.pause_turn(
                        session_id,
                        canonical,
                        history,
                        step_num + 1,
                        branch_points,
                        &emitter,
                        SessionStatus::PausedAwaitingAnswer,
                        &question,
                        None,
                        infer,
                        // The question is re-persisted as a plain assistant
                        // message (fresh id, `is_ask` false so pause_turn
                        // persists it): the row re-seeds the resume canonical.
                        // The review renders the ask CARD from the original
                        // question message (persisted under the ask step's id
                        // at pause time) and drops this fresh bubble by
                        // content match (legacy path).
                        None,
                        false,
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
                    && empty_retries_remaining < self.context_limits.empty_response_max_retries
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
                // Guard against clobbering: when the response carried no text
                // (thought is None after failed retries), `history.last()`
                // points at a PREVIOUS step; only attach the synthesized final
                // to this step's own entry, otherwise the previous step's
                // action/observation is silently overwritten.
                if let Some(last) = history.last_mut().filter(|s| s.step_number == step_num) {
                    last.action = Some(Action {
                        tool_name: "final_answer".into(),
                        tool_input: serde_json::Value::Null,
                        is_final: true,
                        tool_call_id: None,
                    });
                    if last.observation.is_none() {
                        last.observation = Some(msg.clone());
                    }
                } else {
                    history.push(ReActStep {
                        step_number: step_num,
                        thought: Some(msg.clone()),
                        action: Some(Action {
                            tool_name: "final_answer".into(),
                            tool_input: serde_json::Value::Null,
                            is_final: true,
                            tool_call_id: None,
                        }),
                        observation: Some(msg.clone()),
                    });
                }
                // A user message (or background-action result) arrived while the
                // model was generating this answer: deliver it in the gap
                // between the tool calls and the final content instead of
                // deferring it until after the turn completes. The finished
                // answer is persisted so the conversation stays consistent,
                // then the loop re-runs with the new context.
                let before_inject_len = canonical.len();
                if self.inject_pending_context(&ctx, canonical).await {
                    self.deliver_final_with_pending_context(
                        &ctx,
                        &msg,
                        response.reasoning.clone(),
                        canonical,
                        history,
                        branch_points,
                        before_inject_len,
                        // This branch carries no web search and no tool calls,
                        // so no assistant message was pushed yet.
                        false,
                    )
                    .await;
                    continue;
                }
                // Mirror the finished answer into the canonical before the
                // pause: the snapshot then carries the complete conversation
                // in the right order. Without this, the pause snapshot ends
                // right after the tool results and the resume re-seed has to
                // re-insert the answer at the transcript head (sys_end),
                // placing it BEFORE the tool results that preceded it.
                canonical.push(CanonicalMessage::assistant(
                    vec![ContentPart::text(msg.clone())],
                    None,
                    if response.thinking_blocks.is_empty() {
                        response.reasoning.clone()
                    } else {
                        None
                    },
                    Vec::new(),
                    response.thinking_blocks.clone(),
                ));
                let persist_message_id = self.block_msg_id(session_id, step_num, run_id, "thought");
                self.pause_turn(
                    session_id,
                    canonical,
                    history,
                    step_num + 1,
                    branch_points,
                    &emitter,
                    SessionStatus::Paused,
                    &msg,
                    Some(step_num),
                    infer,
                    Some(&persist_message_id),
                    false,
                )
                .await?;
                return Ok(LoopExit::Paused { reason: PauseReason::TurnEnd });
            }

            if let Some(final_action) = actions.iter().find(|a| a.is_final) {
                let final_text = thought.unwrap_or_else(|| "Session completed.".into());
                // Same clobber guard as the empty-actions branch above.
                if let Some(s) = history.last_mut().filter(|s| s.step_number == step_num) {
                    s.action = Some(final_action.clone());
                    if s.observation.is_none() {
                        s.observation = Some(final_text.clone());
                    }
                }
                // The response may already have pushed its own assistant
                // message: a web-search round (pushed above with the search
                // context) or a response mixing real tool calls with the final
                // action (pushed with tool_calls below). In those cases the
                // final text is already in the canonical and must not be
                // duplicated.
                let already_pushed = has_web_search || actions.iter().any(|a| !a.is_final);
                // Same mid-turn delivery as the empty-actions branch: a
                // message that arrived during this final LLM call is injected
                // before the turn ends so it influences the answer.
                let before_inject_len = canonical.len();
                if self.inject_pending_context(&ctx, canonical).await {
                    self.deliver_final_with_pending_context(
                        &ctx,
                        &final_text,
                        response.reasoning.clone(),
                        canonical,
                        history,
                        branch_points,
                        before_inject_len,
                        already_pushed,
                    )
                    .await;
                    continue;
                }
                // Mirror the finished answer into the canonical before the
                // pause (same ordering rationale as the empty-actions branch).
                if !already_pushed {
                    canonical.push(CanonicalMessage::assistant(
                        vec![ContentPart::text(final_text.clone())],
                        None,
                        if response.thinking_blocks.is_empty() {
                            response.reasoning.clone()
                        } else {
                            None
                        },
                        Vec::new(),
                        response.thinking_blocks.clone(),
                    ));
                }
                let persist_message_id = self.block_msg_id(session_id, step_num, run_id, "thought");
                self.pause_turn(
                    session_id,
                    canonical,
                    history,
                    step_num + 1,
                    branch_points,
                    &emitter,
                    SessionStatus::Paused,
                    &final_text,
                    Some(step_num),
                    infer,
                    Some(&persist_message_id),
                    false,
                )
                .await?;
                return Ok(LoopExit::Paused { reason: PauseReason::TurnEnd });
            }

            if let Some(ref t) = thought {
                let text = t.trim();
                if !text.is_empty() {
                    let message_id = self.block_msg_id(session_id, step_num, run_id, "thought");
                    self.persist_session_message(
                        session_id,
                        "assistant",
                        text,
                        Some("text"),
                        None,
                        Some(&message_id),
                    )
                    .await;
                }
            }

            match self
                .execute_tool_batch(
                    session_id,
                    canonical,
                    history,
                    step_num,
                    branch_points,
                    &emitter,
                    infer,
                    run_id,
                    &actions,
                    &thought,
                    &response,
                    &cancel_res,
                    max_steps,
                )
                .await?
            {
                ToolBatchOutcome::Continue => {}
                ToolBatchOutcome::Done(exit) => return Ok(exit),
            }
        }

        self.pause_turn_budget(
            session_id,
            canonical,
            history,
            last_step + 1,
            branch_points,
            &emitter,
            infer,
        )
        .await?;
        Ok(LoopExit::Paused {
            reason: PauseReason::Budget,
        })
    }

}
