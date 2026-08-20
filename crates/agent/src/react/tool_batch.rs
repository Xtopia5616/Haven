//! Tool-batch helpers (failure classify/nudge) and `execute_tool_batch`.
//!
//! Split from `react.rs` (Phase 1 mechanical extract; behavior unchanged).

use super::hooks::BeforeToolAction;
use super::*;
use crate::types::{Action, BranchPoint, ConfirmPending, ConfirmPendingTool, ReActStep};
use haven_common::types::{CanonicalMessage, CanonicalRole, CanonicalToolCall, ContentPart};
use haven_tools::is_silent_action;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;


/// Failure classification used to shape the post-failure retry nudge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum FailureKind {
    /// The environment cannot run the approach: missing command, wrong shell,
    /// network/proxy trouble, bad paths. The approach itself may be sound.
    Environmental,
    /// The approach/usage itself is flawed (bad params, parse failures).
    Logic,
    /// Cannot tell from the error text.
    Unknown,
}

/// Stable identity for a tool call across the action/observation UI pairing,
/// matching the frontend's `tool_call_id || tool_name` id so an interrupted
/// observation lands on the same card the action event opened.
pub(crate) fn tool_key(a: &Action) -> String {
    a.tool_call_id
        .clone()
        .unwrap_or_else(|| a.tool_name.clone())
}

/// `message_inbox` result is an empty poll (`count: 0`): nothing for the
/// user to see, so the observation card is suppressed.
pub(crate) fn empty_inbox_output(result: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(result)
        .ok()
        .and_then(|v| v.get("count").and_then(|c| c.as_u64()))
        == Some(0)
}

impl ReActEngine {
    /// Compose the retry nudge after a step where tool calls failed. The
    /// failure evidence is classified first: environment-type failures
    /// (missing command, wrong shell syntax, network/proxy, paths) must NOT
    /// push the model to abandon its approach — the correct move is to
    /// diagnose and fix the environment (different shell, different tool,
    /// corrected path) and retry. Logic failures get a fix-and-retry nudge
    /// with an explicit threshold before switching approach. This replaces
    /// the old unconditional "try a completely different approach" nudge,
    /// which repeatedly sent users down wrong paths when the real cause was
    /// environmental (Get-FileHash missing in the chosen shell, a broken
    /// proxy, a different 7z path). The generic branch reuses the canonical
    /// guidance from the system prompt (guideline 12) so the two cannot
    /// drift.
    pub(super) fn build_failure_nudge(failures: &[(String, String)]) -> String {
        let has_env = failures
            .iter()
            .any(|(t, e)| Self::classify_tool_failure(t, e) == FailureKind::Environmental);
        let has_logic = failures
            .iter()
            .any(|(t, e)| Self::classify_tool_failure(t, e) == FailureKind::Logic);
        if has_env {
            "The tool failures look ENVIRONMENTAL (missing command / wrong shell syntax / network / path), not logic errors. Do NOT abandon your approach. Diagnose the environment first: verify the command exists in the shell you chose (cmd vs PowerShell syntax differs; `&&` only works in cmd), check network/proxy/endpoints, fix paths and prerequisites. Switching tools (e.g. curl -> aria2) or shells is an environment fix, not a change of approach — keep the same approach and retry."
                .into()
        } else if has_logic {
            "The previous approach failed with logic errors. Analyze the exact error, fix the specific mistake, and retry. Only consider a completely different approach if the same method fails again after you fixed it."
                .into()
        } else {
            format!(
                "The previous approach encountered errors. {}",
                haven_common::prompts::TOOL_FAILURE_DIAGNOSIS
            )
        }
    }

    /// Heuristic classification of a tool failure: environment problems (the
    /// user's tools/environment cannot run the approach) vs logic problems
    /// (the approach itself is flawed). Used to shape the retry nudge so
    /// environmental failures do not trigger an unnecessary method switch.
    pub(super) fn classify_tool_failure(tool_name: &str, err: &str) -> FailureKind {
        // Tool-usage mistakes by the model itself (missing params, invalid
        // input) are logic errors: the schema/validation error names the fix.
        if tool_name == "files"
            && (err.contains("MISSING REQUIRED FIELD")
                || err.contains("old_string")
                || err.contains("not found in file"))
        {
            return FailureKind::Logic;
        }
        let e = err.to_lowercase();
        const ENV_MARKERS: &[&str] = &[
            // command / executable missing
            "not recognized",
            "not recognized as an internal or external command",
            "不是内部或外部命令",
            "command not found",
            "无法识别",
            "not found",
            "cannot be found",
            "cannot find",
            "找不到",
            "no such file",
            "no such directory",
            "spawn",
            "program not found",
            // network / proxy / transport
            "connection",
            "timed out",
            "timeout",
            "refused",
            "reset",
            "proxy",
            "unreachable",
            "resolve",
            "dns",
            "ssl",
            "tls",
            "certificate",
            "failed to connect",
            "tunnel",
            "network",
            // paths / permissions
            "path does not exist",
            "路径不存在",
            "access denied",
            "拒绝访问",
            // PowerShell/7z style environment mismatches
            "无法将",
            "不是有效的",
        ];
        if ENV_MARKERS.iter().any(|m| e.contains(m)) {
            return FailureKind::Environmental;
        }
        const LOGIC_MARKERS: &[&str] = &[
            "validation failed",
            "missing required",
            "parse error",
            "syntax error",
            "unterminated",
            "invalid json",
            "is required for",
        ];
        if LOGIC_MARKERS.iter().any(|m| e.contains(m)) {
            return FailureKind::Logic;
        }
        FailureKind::Unknown
    }
}

/// Outcome of one tool batch: continue the step loop, or exit the run with
/// an explicit [`LoopExit`] (Phase 2 / C2).
pub(super) enum ToolBatchOutcome {
    Continue,
    Done(LoopExit),
}

impl ReActEngine {
    /// Execute the non-final actions for one step: emit Action cards, run the
    /// batch (parallel), drain observations, failure nudge, and ask pause.
    /// Behavior-preserving extract from `run_react_loop` (Phase 1 / E2).
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn execute_tool_batch(
        &self,
        session_id: &str,
        canonical: &mut Vec<CanonicalMessage>,
        history: &mut Vec<ReActStep>,
        step_num: u32,
        branch_points: &mut HashMap<u32, BranchPoint>,
        emitter: &Arc<dyn AgentEventEmitter>,
        infer: &(dyn Fn(bool) + Send + Sync),
        run_id: u64,
        actions: &[Action],
        thought: &Option<String>,
        response: &haven_llm::LlmResponse,
        cancel_res: &tokio_util::sync::CancellationToken,
        max_steps: u32,
    ) -> anyhow::Result<ToolBatchOutcome> {
        let non_final: Vec<&Action> = actions.iter().filter(|a| !a.is_final).collect();
        // Mint one `step-*` id per action, shared by the Action event,
        // the tool's step row (created inside execute_step) and the
        // Observation event, so the live card and the review badge (both
        // keyed `step-<id>`) are one entity. The ids are indexed by the
        // action's position in `non_final` (NOT by `tool_call_id`, which
        // two actions of a malformed provider response could share —
        // keying by it would collapse both onto one step id and the
        // second step-row insert would fail the PRIMARY KEY).
        let action_step_ids: Vec<String> = non_final
            .iter()
            .map(|_| haven_common::types::new_id("step"))
            .collect();
        for (idx, action) in non_final.iter().enumerate() {
            let step_id = &action_step_ids[idx];
            // Persist the pending step row BEFORE the live card so an
            // interrupt / Continue resync / app restart can rebuild it
            // from session_steps (the card used to be live-only and
            // vanished on every DB rebuild).
            self.executor
                .begin_action_step(
                    session_id,
                    &action.tool_name,
                    &action.tool_input,
                    step_num,
                    step_id,
                )
                .await;
            emitter
                .emit(crate::event::AgentEvent::Action {
                    session_id: session_id.into(),
                    tool_name: action.tool_name.clone(),
                    input: action.tool_input.clone(),
                    step_number: step_num,
                    run_id,
                    tool_call_id: action.tool_call_id.clone(),
                    step_id: step_id.clone(),
                })
                .await;
        }

        if !non_final.is_empty() {
            // The tool_calls echoed into the canonical assistant message
            // must exactly match the tool results pushed below, or
            // providers reject the request with a 400. They are built
            // from the ACTIONS (not `response.tool_calls`) so that a
            // retry-replaced response stays consistent: when the empty /
            // cut-off retry produced the tool calls, the original
            // `response.tool_calls` is empty and zipping it with the
            // retried actions would emit an assistant message WITHOUT
            // tool_calls followed by orphaned tool results (silently
            // dropped by sanitize_canonical, losing the observations).
            // The Action side already carries the synthesized UUID for
            // empty provider ids, matching the tool-result side below.
            let tool_calls: Option<Vec<CanonicalToolCall>> = Some(
                non_final
                    .iter()
                    .map(|a| CanonicalToolCall {
                        id: a.tool_call_id.clone().unwrap_or_default(),
                        name: a.tool_name.clone(),
                        arguments: a.tool_input.clone(),
                    })
                    .collect(),
            );
            // The text must match what `persist_session_message` stores
            // (trimmed thought) so resume dedup cannot fail; a
            // retry-replaced response also must not echo the cut-off
            // original text.
            let push_text = thought.as_deref().unwrap_or(&response.text);
            // A response mixing real tool calls with a web search round
            // carries both: the `web_search_call` items round-trip in the
            // same assistant message so the next request restores the
            // search context alongside the function tool results.
            canonical.push(CanonicalMessage::assistant(
                vec![ContentPart::text(push_text.to_string())],
                tool_calls,
                if response.thinking_blocks.is_empty() {
                    response.reasoning.clone()
                } else {
                    None
                },
                response.web_search_calls.clone(),
                response.thinking_blocks.clone(),
            ));
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

        use futures_util::StreamExt;

        // Phase 5 / E3: pre-check every non-final action before spawning.
        // Proceed tools run in parallel; Block writes a failure observation
        // immediately; NeedConfirm is collected and pauses after the drain.
        let gate_ctx = StepCtx {
            session_id: session_id.to_string(),
            step_num,
            run_id,
            emitter: emitter.clone(),
        };
        let mut need_confirm: Vec<ConfirmPendingTool> = Vec::new();
        let mut proceed: Vec<(usize, Action, Option<bool>)> = Vec::new();
        let mut any_tool_failure = false;
        // Bounded per-step failure evidence (tool name + error tail) used
        // to classify failures as environmental vs logic when composing
        // the retry nudge — a broken proxy or a missing command must not
        // push the model to abandon a sound approach.
        let mut failure_signals: Vec<(String, String)> = Vec::new();
        // Tool calls in this batch that already produced a result, keyed
        // by the same identity the action/observation pairing uses. When
        // the batch is cancelled mid-flight, every `non_final` action NOT
        // in this set was cut off — it must still be repaired with an
        // "Interrupted" result and surfaced, not silently dropped.
        let mut completed_tool_keys: HashSet<String> = HashSet::new();
        // If the agent invoked the `ask` tool, the session must pause and
        // wait for the user's reply (delivered as a supplement). Collect
        // every question in the batch so all are surfaced, plus the step
        // row id of each ask action: the question message is persisted
        // under that id so the ask card and its content share one entity.
        let mut asked_questions: Vec<String> = Vec::new();
        let mut ask_step_ids: Vec<String> = Vec::new();

        for (idx, action) in non_final.iter().enumerate() {
            match self
                .hooks
                .before_tool(self, &gate_ctx, &action.tool_name, &action.tool_input)
                .await
            {
                BeforeToolAction::Proceed { confirmed } => {
                    proceed.push((idx, (*action).clone(), confirmed));
                }
                BeforeToolAction::Block { error } => {
                    any_tool_failure = true;
                    if failure_signals.len() < 3 {
                        let cap: String = error.chars().take(600).collect();
                        failure_signals.push((action.tool_name.clone(), cap));
                    }
                    let step_id = action_step_ids[idx].clone();
                    let silent = is_silent_action(&action.tool_name, &action.tool_input);
                    self.executor
                        .finish_interrupted_step(
                            session_id,
                            &action.tool_name,
                            &action.tool_input,
                            step_num,
                            &step_id,
                            &error,
                        )
                        .await;
                    emitter
                        .emit(crate::event::AgentEvent::Observation {
                            session_id: session_id.into(),
                            observation: error.clone(),
                            tool_name: action.tool_name.clone(),
                            step_number: step_num,
                            run_id,
                            silent,
                            tool_call_id: action.tool_call_id.clone(),
                            ask_options: Vec::new(),
                            step_id,
                        })
                        .await;
                    if let Some(last) = history
                        .last_mut()
                        .filter(|s| s.step_number == step_num && s.action.is_none())
                    {
                        last.action = Some((*action).clone());
                        last.observation = Some(error.clone());
                    } else {
                        history.push(ReActStep {
                            step_number: step_num,
                            thought: None,
                            action: Some((*action).clone()),
                            observation: Some(error.clone()),
                        });
                    }
                    canonical.push(CanonicalMessage::tool(
                        vec![ContentPart::text(error)],
                        action.tool_call_id.clone(),
                    ));
                    completed_tool_keys.insert(tool_key(action));
                }
                BeforeToolAction::NeedConfirm { risk_level } => {
                    need_confirm.push(ConfirmPendingTool {
                        confirm_id: haven_common::types::new_id("conf"),
                        tool_name: action.tool_name.clone(),
                        tool_input: action.tool_input.clone(),
                        step_id: action_step_ids[idx].clone(),
                        risk_level,
                        decision: None,
                    });
                }
            }
        }

        let mut tool_futures = futures_util::stream::FuturesUnordered::new();
        for (idx, action, confirmed) in proceed {
            let session_id = session_id.to_string();
            let tool_name = action.tool_name.clone();
            let tool_input = action.tool_input.clone();
            let max_obs = self.context_limits.max_observation_chars;
            let executor = self.executor.clone();
            // The same step id minted at Action-emit time keys the step
            // row execute_step creates, so the live card id, the DB badge
            // id and this step id are identical everywhere.
            let step_id = action_step_ids[idx].clone();
            let pre_confirmed = confirmed == Some(true);
            tool_futures.push(async move {
                tracing::debug!(
                    "executing tool '{}' at step {} (input keys: {:?})",
                    tool_name,
                    step_num,
                    tool_input
                        .as_object()
                        .map(|o| o.keys().collect::<Vec<_>>())
                        .unwrap_or_default()
                );
                tracing::trace!(
                    "tool '{}' at step {} full input: {} chars",
                    tool_name,
                    step_num,
                    tool_input
                        .as_object()
                        .map(|o| serde_json::to_string(o).map(|s| s.len()).unwrap_or(0))
                        .unwrap_or(0)
                );
                let result = if pre_confirmed {
                    executor
                        .execute_step_preconfirmed(
                            &session_id,
                            &tool_name,
                            tool_input.clone(),
                            step_num,
                            &step_id,
                            true,
                        )
                        .await
                } else {
                    executor
                        .execute_step(
                            &session_id,
                            &tool_name,
                            tool_input.clone(),
                            step_num,
                            &step_id,
                        )
                        .await
                };
                let (text, is_error, ask_question, ask_options, notify_title, notify_body) =
                    match result {
                        Ok(r) => {
                            tracing::debug!(
                                "tool '{}' at step {} completed: success={}, {} chars",
                                tool_name,
                                step_num,
                                r.success,
                                serde_json::to_string(&r.output)
                                    .map(|s| s.len())
                                    .unwrap_or(0)
                            );
                            tracing::trace!(
                                "tool '{}' at step {} full output: {} chars",
                                tool_name,
                                step_num,
                                serde_json::to_string(&r.output)
                                    .map(|s| s.len())
                                    .unwrap_or(0)
                            );
                            let text = r.summary_text();
                            let text = if text.len() > max_obs {
                                let cutoff = text.floor_char_boundary(max_obs);
                                format!(
                                    "{}[... truncated {} chars omitted]",
                                    &text[..cutoff],
                                    text.len() - cutoff
                                )
                            } else {
                                text
                            };
                            // The ask/notify signals are attached to the
                            // result by the tool itself (declared via
                            // `Tool::signals`) BEFORE the loop truncates
                            // the observation text, so a question or toast
                            // is never lost to the budget.
                            let ask_question = r.signals.ask_question.clone();
                            let ask_options = r.signals.ask_options.clone();
                            let notify_title = r.signals.notify_title.clone();
                            let notify_body = r.signals.notify_body.clone();
                            (
                                text,
                                !r.success,
                                ask_question,
                                ask_options,
                                notify_title,
                                notify_body,
                            )
                        }
                        Err(e) => {
                            tracing::debug!(
                                "tool '{}' at step {} failed: {}",
                                tool_name,
                                step_num,
                                e
                            );
                            (e.to_string(), true, None, Vec::new(), None, None)
                        }
                    };
                (
                    action,
                    tool_name,
                    text,
                    is_error,
                    ask_question,
                    ask_options,
                    notify_title,
                    notify_body,
                    step_id,
                )
            });
        }

        // Drain tool results while remaining responsive to cancellation.
        // Without select!, a cancel arriving mid-batch would only be
        // detected at the next step boundary —after all tools finish.
        loop {
            tokio::select! {
                biased;
                _ = cancel_res.cancelled() => {
                    tracing::info!("ReAct loop cancelled during tool batch at step {}", step_num);
                    // Tool calls still in flight were cut off, not skipped:
                    // repair EACH one with an "Interrupted" result so the
                    // model sees the tool was attempted (and may retry it),
                    // and surface it in the UI as an interrupted
                    // observation card rather than leaving a silent gap.
                    for (idx, action) in non_final.iter().enumerate() {
                        if completed_tool_keys.contains(&tool_key(action)) {
                            continue;
                        }
                        let silent_action =
                            is_silent_action(&action.tool_name, &action.tool_input);
                        let interrupted_text = crate::interrupted_result_text(
                            &action.tool_name,
                            &action.tool_input,
                        );
                        // Complete the pending step row minted at Action
                        // time so review/resume rebuilds the Interrupted
                        // card from session_steps (not live-only).
                        let step_id = action_step_ids[idx].clone();
                        self.executor
                            .finish_interrupted_step(
                                session_id,
                                &action.tool_name,
                                &action.tool_input,
                                step_num,
                                &step_id,
                                &interrupted_text,
                            )
                            .await;
                        emitter
                            .emit(crate::event::AgentEvent::Observation {
                                session_id: session_id.into(),
                                observation: interrupted_text.clone(),
                                tool_name: action.tool_name.clone(),
                                step_number: step_num,
                                run_id,
                                silent: silent_action,
                                tool_call_id: action.tool_call_id.clone(),
                                ask_options: Vec::new(),
                                step_id,
                            })
                            .await;
                        canonical.push(CanonicalMessage::tool(
                            vec![ContentPart::text(interrupted_text.clone())],
                            action.tool_call_id.clone(),
                        ));
                        if let Some(step) = history
                            .iter_mut()
                            .find(|s| s.step_number == step_num && s.action.is_none())
                        {
                            step.action = Some((*action).clone());
                            step.observation = Some(interrupted_text);
                        } else {
                            history.push(ReActStep {
                                step_number: step_num,
                                thought: None,
                                action: Some((*action).clone()),
                                observation: Some(interrupted_text),
                            });
                        }
                    }
                    // A rollback that lands mid-batch must find the DB row
                    // at the pre-batch branch point (the response and
                    // partial tool results are discarded by the exit).
                    self.save_exit_snapshot(
                        session_id,
                        canonical,
                        history,
                        step_num,
                        branch_points,
                    )
                    .await;
                    return Ok(ToolBatchOutcome::Done(LoopExit::Cancelled));
                }
                item = tool_futures.next() => {
                    let Some((
                        action,
                        tool_name,
                        step_result,
                        is_error,
                        ask_question,
                        ask_options,
                        notify_title,
                        notify_body,
                        step_id,
                    )) = item
                    else {
                        break;
                    };
                    if is_error {
                        any_tool_failure = true;
                        if failure_signals.len() < 3 {
                            let cap: String = step_result.chars().take(600).collect();
                            failure_signals.push((tool_name.clone(), cap));
                        }
                    }
                    // The `notify` tool requests a user-facing notification:
                    // emit it (in-app toast + Windows) without pausing the
                    // ReAct loop.
                    if let (Some(title), Some(body)) = (&notify_title, &notify_body) {
                        emitter
                            .emit(crate::event::AgentEvent::Notification {
                                session_id: session_id.into(),
                                title: title.clone(),
                                body: body.clone(),
                            })
                            .await;
                    }
                    // Surface an `ask` result as a readable question rather
                    // than raw JSON. The user's reply arrives via
                    // process_input —supplement —Paused → Pending resume.
                    if let Some(q) = &ask_question {
                        asked_questions.push(q.clone());
                        ask_step_ids.push(step_id.clone());
                    }
                    // `ask` must never be silent: hiding the question
                    // while the session pauses for an answer would leave the
                    // user waiting on a question they can't see.
                    let silent = is_silent_action(&tool_name, &action.tool_input)
                        // An empty message_inbox poll carries no user
                        // information — hide the card instead of spamming
                        // the chat on every routine check.
                        || (tool_name == "message_inbox"
                            && empty_inbox_output(&step_result));
                    // For `ask`, the chat/review bubble shows the readable
                    // question text; the canonical (model) context keeps
                    // the raw JSON so the model can still parse the flag.
                    // Same for `notify`: show a readable confirmation
                    // instead of the raw signal JSON.
                    let display_observation = if let Some(q) = &ask_question {
                        q.clone()
                    } else if let Some(title) = &notify_title {
                        let body = notify_body.clone().unwrap_or_default();
                        if body.is_empty() {
                            step_result.clone()
                        } else {
                            format!("Notification sent: {title}: {body}")
                        }
                    } else {
                        step_result.clone()
                    };
                    emitter
                        .emit(crate::event::AgentEvent::Observation {
                            session_id: session_id.into(),
                            observation: display_observation.clone(),
                            tool_name: tool_name.clone(),
                            step_number: step_num,
                            run_id,
                            silent,
                            tool_call_id: action.tool_call_id.clone(),
                            ask_options: ask_options.clone(),
                            step_id,
                        })
                        .await;

                    if let Some(last) = history
                        .last_mut()
                        .filter(|s| s.step_number == step_num && s.action.is_none())
                    {
                        // First tool result of this step: fill the thought
                        // entry pushed at step start.
                        last.action = Some(action.clone());
                        last.observation = Some(display_observation.clone());
                    } else {
                        // A later tool of a multi-tool step, or a tool-only
                        // step (thought was None, so no entry was pushed at
                        // step start): append a fresh entry instead of
                        // overwriting the previous entry. The old behavior
                        // kept only the LAST completed tool per step (and
                        // could clobber the PREVIOUS step's entry when the
                        // response carried no thought), silently dropping
                        // every other tool from the step history — which
                        // also made restore_per_session_tools miss parallel
                        // load_skill/load_mcp registrations on restart.
                        history.push(ReActStep {
                            step_number: step_num,
                            thought: None,
                            action: Some(action.clone()),
                            observation: Some(display_observation),
                        });
                    }

                    canonical.push(CanonicalMessage::tool(
                        vec![ContentPart::text(step_result)],
                        action.tool_call_id.clone(),
                    ));
                    completed_tool_keys.insert(tool_key(&action));
                }
            }
        }

        // Skip the retry nudge when the batch asked the user or is about to
        // pause for confirm: it would be baked into the paused snapshot ahead
        // of the user's real answer / decision.
        if any_tool_failure
            && asked_questions.is_empty()
            && need_confirm.is_empty()
            && step_num < max_steps - 1
        {
            canonical.push(CanonicalMessage::user_text(Self::build_failure_nudge(
                &failure_signals,
            )));
        }

        // Phase 5 / E3: confirm before ask when both appear in one batch.
        // Ask pause used to return first and drop NeedConfirm tools (Action
        // cards + assistant tool_calls with no results → Interrupted repair).
        // Prefer confirm pause; stash ask pending so finish_confirm_batch's
        // next turn still surfaces the question.
        if !need_confirm.is_empty() {
            if !asked_questions.is_empty() {
                let question = asked_questions.join("\n\n");
                for (i, q) in asked_questions.iter().enumerate() {
                    let msg_id = ask_step_ids
                        .get(i)
                        .cloned()
                        .unwrap_or_else(|| haven_common::types::new_id("step"));
                    self.persist_session_message(
                        session_id,
                        "assistant",
                        q,
                        Some("text"),
                        None,
                        Some(&msg_id),
                    )
                    .await;
                }
                self.executor
                    .set_awaiting_answer(
                        session_id,
                        Some(crate::types::AskPending {
                            question,
                            step_ids: ask_step_ids.clone(),
                        }),
                    )
                    .await;
            }
            let pending = ConfirmPending {
                step_number: step_num,
                tools: need_confirm,
            };
            self.executor
                .request_confirm_batch(session_id, pending)
                .await;
            self.pause_turn(
                session_id,
                canonical,
                history,
                step_num + 1,
                branch_points,
                emitter,
                SessionStatus::PausedAwaitingConfirm,
                "Waiting for confirmation…",
                None,
                infer,
                None,
                false,
            )
            .await?;
            return Ok(ToolBatchOutcome::Done(LoopExit::Paused {
                reason: PauseReason::Confirm,
            }));
        }

        // The agent asked the human a question: pause so the user can
        // answer. Their reply arrives as a supplement and resumes the session
        // (Paused —Pending —dispatcher re-enters the loop, injecting the
        // answer as context at the top of the next step).
        if !asked_questions.is_empty() {
            let question = asked_questions.join("\n\n");
            // Persist one question message per ask step, each under the
            // step row's id: the message row is the ask card's content
            // authority (the step row only carries execution state), and
            // the shared id lets the review builder link them without
            // content matching or a sentinel. The message row also
            // re-seeds the question into the canonical on resume. A
            // defensive fresh id keeps the question visible even if a
            // step row is missing.
            for (i, q) in asked_questions.iter().enumerate() {
                let msg_id = ask_step_ids
                    .get(i)
                    .cloned()
                    .unwrap_or_else(|| haven_common::types::new_id("step"));
                self.persist_session_message(
                    session_id,
                    "assistant",
                    q,
                    Some("text"),
                    None,
                    Some(&msg_id),
                )
                .await;
            }
            // Phase 4 / C3: no steering→answer queue transfer. Mid-run user
            // input landed in steering while status was still Running; mark
            // those (and any follow-ups) as answers in place. Only set the
            // explicit awaiting flag (C5) when no reply is queued yet —
            // otherwise Pending + is_answer inject clears the gate without
            // leaving a stale snapshot flag that could resurrect after crash.
            self.executor.mark_user_queues_as_answer(session_id).await;
            let has_answer = self.executor.has_pending_context(session_id).await;
            let status = if has_answer {
                self.executor.clear_awaiting_answer(session_id).await;
                SessionStatus::Pending
            } else {
                self.executor
                    .set_awaiting_answer(
                        session_id,
                        Some(crate::types::AskPending {
                            question: question.clone(),
                            step_ids: ask_step_ids.clone(),
                        }),
                    )
                    .await;
                SessionStatus::PausedAwaitingAnswer
            };
            self.pause_turn(
                session_id,
                canonical,
                history,
                step_num + 1,
                branch_points,
                &emitter,
                status,
                &question,
                None,
                infer,
                // The question messages were persisted above (one per ask
                // step, under the step ids); `is_ask` tells pause_turn to
                // skip its own persist.
                None,
                true,
            )
            .await?;
            return Ok(ToolBatchOutcome::Done(LoopExit::Paused {
                reason: PauseReason::Ask,
            }));
        }

        let state = self.executor.get_session_state(session_id).await;
        match state {
            Some(s) if s.is_paused() => {
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
                    run_id: 0,
                    emitter: emitter.clone(),
                };
                self.hooks
                    .on_pause(self, &ctx, PauseReason::External, infer)
                    .await;
                return Ok(ToolBatchOutcome::Done(LoopExit::Paused {
                    reason: PauseReason::External,
                }));
            }
            Some(SessionStatus::Error) => {
                self.save_exit_snapshot(
                    session_id,
                    canonical,
                    history,
                    step_num,
                    branch_points,
                )
                .await;
                return Ok(ToolBatchOutcome::Done(LoopExit::Error(
                    "session interrupted".into(),
                )));
            }
            // Session gone (end_session/terminal cleanup) or completed: exit.
            None | Some(SessionStatus::Completed) => {
                self.save_exit_snapshot(
                    session_id,
                    canonical,
                    history,
                    step_num,
                    branch_points,
                )
                .await;
                return Ok(ToolBatchOutcome::Done(LoopExit::Completed));
            }
            _ => {}
        }

        Ok(ToolBatchOutcome::Continue)
    }

    /// Resume after a confirm pause: execute decided gated tools without
    /// re-emitting Action cards (those were already shown when the batch
    /// paused). Appends observations / history / canonical for each tool.
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn finish_confirm_batch(
        &self,
        session_id: &str,
        canonical: &mut Vec<CanonicalMessage>,
        history: &mut Vec<ReActStep>,
        branch_points: &mut HashMap<u32, BranchPoint>,
        emitter: &Arc<dyn AgentEventEmitter>,
        infer: &(dyn Fn(bool) + Send + Sync),
        run_id: u64,
    ) -> anyhow::Result<ToolBatchOutcome> {
        let Some(pending) = self.executor.get_awaiting_confirm(session_id).await else {
            return Ok(ToolBatchOutcome::Continue);
        };
        let step_num = pending.step_number;
        let max_obs = self.context_limits.max_observation_chars;

        for tool in &pending.tools {
            let Some(decision) = tool.decision else {
                continue;
            };
            let tool_call_id = tool_call_id_for(canonical, &tool.tool_name, &tool.tool_input);
            let action = Action {
                tool_name: tool.tool_name.clone(),
                tool_input: tool.tool_input.clone(),
                is_final: false,
                tool_call_id: tool_call_id.clone(),
            };

            if decision {
                let result = self
                    .executor
                    .execute_step_preconfirmed(
                        session_id,
                        &tool.tool_name,
                        tool.tool_input.clone(),
                        step_num,
                        &tool.step_id,
                        true,
                    )
                    .await;
                let (text, is_error, ask_question, ask_options, notify_title, notify_body) =
                    match result {
                        Ok(r) => {
                            let text = r.summary_text();
                            let text = if text.len() > max_obs {
                                let cutoff = text.floor_char_boundary(max_obs);
                                format!(
                                    "{}[... truncated {} chars omitted]",
                                    &text[..cutoff],
                                    text.len() - cutoff
                                )
                            } else {
                                text
                            };
                            (
                                text,
                                !r.success,
                                r.signals.ask_question.clone(),
                                r.signals.ask_options.clone(),
                                r.signals.notify_title.clone(),
                                r.signals.notify_body.clone(),
                            )
                        }
                        Err(e) => (e.to_string(), true, None, Vec::new(), None, None),
                    };
                let _ = is_error;
                if let (Some(title), Some(body)) = (&notify_title, &notify_body) {
                    emitter
                        .emit(crate::event::AgentEvent::Notification {
                            session_id: session_id.into(),
                            title: title.clone(),
                            body: body.clone(),
                        })
                        .await;
                }
                let silent = is_silent_action(&tool.tool_name, &tool.tool_input)
                    || (tool.tool_name == "message_inbox" && empty_inbox_output(&text));
                let display_observation = if let Some(q) = &ask_question {
                    q.clone()
                } else if let Some(title) = &notify_title {
                    let body = notify_body.clone().unwrap_or_default();
                    if body.is_empty() {
                        text.clone()
                    } else {
                        format!("Notification sent: {title}: {body}")
                    }
                } else {
                    text.clone()
                };
                emitter
                    .emit(crate::event::AgentEvent::Observation {
                        session_id: session_id.into(),
                        observation: display_observation.clone(),
                        tool_name: tool.tool_name.clone(),
                        step_number: step_num,
                        run_id,
                        silent,
                        tool_call_id: tool_call_id.clone(),
                        ask_options,
                        step_id: tool.step_id.clone(),
                    })
                    .await;
                if let Some(last) = history
                    .last_mut()
                    .filter(|s| s.step_number == step_num && s.action.is_none())
                {
                    last.action = Some(action.clone());
                    last.observation = Some(display_observation);
                } else {
                    history.push(ReActStep {
                        step_number: step_num,
                        thought: None,
                        action: Some(action),
                        observation: Some(display_observation),
                    });
                }
                canonical.push(CanonicalMessage::tool(
                    vec![ContentPart::text(text)],
                    tool_call_id,
                ));
            } else {
                let error = format!(
                    "The user REJECTED the operation '{}' (confirmation declined). Do NOT retry it — ask the user what to do instead or choose a different approach.",
                    tool.tool_name
                );
                let silent = is_silent_action(&tool.tool_name, &tool.tool_input);
                self.executor
                    .finish_interrupted_step(
                        session_id,
                        &tool.tool_name,
                        &tool.tool_input,
                        step_num,
                        &tool.step_id,
                        &error,
                    )
                    .await;
                emitter
                    .emit(crate::event::AgentEvent::Observation {
                        session_id: session_id.into(),
                        observation: error.clone(),
                        tool_name: tool.tool_name.clone(),
                        step_number: step_num,
                        run_id,
                        silent,
                        tool_call_id: tool_call_id.clone(),
                        ask_options: Vec::new(),
                        step_id: tool.step_id.clone(),
                    })
                    .await;
                if let Some(last) = history
                    .last_mut()
                    .filter(|s| s.step_number == step_num && s.action.is_none())
                {
                    last.action = Some(action.clone());
                    last.observation = Some(error.clone());
                } else {
                    history.push(ReActStep {
                        step_number: step_num,
                        thought: None,
                        action: Some(action),
                        observation: Some(error.clone()),
                    });
                }
                canonical.push(CanonicalMessage::tool(
                    vec![ContentPart::text(error)],
                    tool_call_id,
                ));
            }
        }

        self.executor
            .clear_awaiting_confirm_persisted(session_id)
            .await;

        // Same-batch ask was stashed while confirm paused first: surface it now.
        if let Some(ask) = self.executor.get_awaiting_answer(session_id).await {
            self.executor.mark_user_queues_as_answer(session_id).await;
            let has_answer = self.executor.has_pending_context(session_id).await;
            let status = if has_answer {
                self.executor.clear_awaiting_answer(session_id).await;
                SessionStatus::Pending
            } else {
                SessionStatus::PausedAwaitingAnswer
            };
            self.pause_turn(
                session_id,
                canonical,
                history,
                step_num + 1,
                branch_points,
                emitter,
                status,
                &ask.question,
                None,
                infer,
                None,
                true,
            )
            .await?;
            return Ok(ToolBatchOutcome::Done(LoopExit::Paused {
                reason: PauseReason::Ask,
            }));
        }

        Ok(ToolBatchOutcome::Continue)
    }
}

fn tool_call_id_for(
    canonical: &[CanonicalMessage],
    tool_name: &str,
    tool_input: &serde_json::Value,
) -> Option<String> {
    for msg in canonical.iter().rev() {
        if msg.role != CanonicalRole::Assistant {
            continue;
        }
        if let Some(calls) = &msg.tool_calls
            && let Some(call) = calls
                .iter()
                .find(|c| c.name == tool_name && c.arguments == *tool_input)
        {
            return Some(call.id.clone());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_inbox_output_detects_only_empty_polls() {
        assert!(empty_inbox_output(r#"{"count": 0, "messages": []}"#));
        assert!(empty_inbox_output(r#"{"count":0}"#));
        assert!(!empty_inbox_output(
            r#"{"count": 1, "messages": [{"id": "msg-x"}]}"#
        ));
        assert!(!empty_inbox_output("not json"));
    }

    #[test]
    fn classify_environmental_vs_logic() {
        assert_eq!(
            ReActEngine::classify_tool_failure(
                "shell",
                "'Get-FileHash' is not recognized as the name of a cmdlet"
            ),
            FailureKind::Environmental
        );
        assert_eq!(
            ReActEngine::classify_tool_failure(
                "files",
                "input validation failed for 'files': MISSING REQUIRED FIELD(S): operation"
            ),
            FailureKind::Logic
        );
        assert_eq!(
            ReActEngine::classify_tool_failure("shell", "something odd happened"),
            FailureKind::Unknown
        );
    }

    #[test]
    fn tool_batch_outcome_variants_exist() {
        let _ = ToolBatchOutcome::Continue;
        let _ = ToolBatchOutcome::Done(LoopExit::Cancelled);
    }
}
