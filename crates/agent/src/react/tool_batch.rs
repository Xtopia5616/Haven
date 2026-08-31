//! Tool-batch helpers (failure classify/nudge) and `execute_tool_batch`.
//!
//! Tool execution is concurrent, but transcript materialization is ordered by
//! the assistant's tool-call list so the next model request is deterministic.

use super::hooks::{BeforeToolAction, ToolCallIdentity};
use super::snapshot_io::PauseTurnInput;
use super::*;
use crate::types::{Action, ConfirmPending, ConfirmPendingTool};
use haven_common::types::{CanonicalMessage, CanonicalRole, CanonicalToolCall, ContentPart};
use haven_memory::repositories::session_steps::ActionStepOutcome;
use haven_tools::{ToolConcurrency, is_silent_action};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::{
    Mutex as AsyncMutex, OwnedMutexGuard, OwnedRwLockReadGuard, OwnedRwLockWriteGuard, RwLock,
};

/// Hard safety ceilings for one assistant response. The first bounds total
/// work admitted to the runtime; the second bounds live futures/tasks. The
/// provider-facing tool-definition limit is not a runtime execution limit.
pub(crate) const MAX_RUNTIME_TOOL_CALLS_PER_BATCH: usize = 64;
const MAX_CONCURRENT_TOOL_CALLS: usize = 8;

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

/// `agent` operation=inbox result is an empty poll (`count: 0`): nothing for
/// the user to see, so the observation card is suppressed.
pub(crate) fn empty_inbox_output(result: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(result)
        .ok()
        .and_then(|v| v.get("count").and_then(|c| c.as_u64()))
        == Some(0)
}

/// True when this is an `agent` inbox poll (check tool_input.operation).
pub(crate) fn is_agent_inbox_call(tool_name: &str, tool_input: &serde_json::Value) -> bool {
    tool_name == "agent" && tool_input.get("operation").and_then(|v| v.as_str()) == Some("inbox")
}

#[derive(Default)]
struct ToolBatchState {
    any_tool_failure: bool,
    failure_signals: Vec<(String, String)>,
    last_failed_tool_call_id: Option<String>,
    asked_questions: Vec<String>,
    ask_step_ids: Vec<String>,
}

impl ToolBatchState {
    async fn commit_tool_result(
        &mut self,
        engine: &ReActEngine,
        ctx: &StepCtx,
        result: CompletedTool,
        state: &mut ReActState,
    ) {
        let CompletedTool {
            action,
            tool_name,
            step_result,
            is_error,
            ask_question,
            ask_options,
            notify_title,
            notify_body,
            step_id,
            action_index,
        } = result;

        if is_error {
            self.any_tool_failure = true;
            self.last_failed_tool_call_id = action.tool_call_id.clone();
            if self.failure_signals.len() < 3 {
                let cap: String = step_result.chars().take(600).collect();
                self.failure_signals.push((tool_name.clone(), cap));
            }
        }
        if let (Some(title), Some(body)) = (&notify_title, &notify_body) {
            ctx.emitter
                .emit(crate::event::AgentEvent::Notification {
                    session_id: ctx.session_id.clone(),
                    title: title.clone(),
                    body: body.clone(),
                })
                .await;
        }
        if let Some(question) = &ask_question {
            self.asked_questions.push(question.clone());
            self.ask_step_ids.push(step_id.clone());
        }

        let tool_call_id = action.tool_call_id.clone();
        let silent = is_silent_action(&tool_name, &action.tool_input)
            || (is_agent_inbox_call(&tool_name, &action.tool_input)
                && empty_inbox_output(&step_result));
        let display_observation = if let Some(question) = &ask_question {
            question.clone()
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
        engine
            .apply_transcript(
                ctx,
                TranscriptEvent::ToolResult {
                    canonical_observation: step_result,
                    history_observation: display_observation,
                    tool_call_id: tool_call_id.clone(),
                    action,
                    action_index,
                    step_id: step_id.clone(),
                    observation_card: Some(ObservationCard {
                        tool_name,
                        tool_call_id,
                        step_id,
                        action_index,
                        silent,
                        ask_options,
                    }),
                },
                state,
            )
            .await;
    }
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
    ///
    /// Phase 7 / G5: the returned text is appended onto the last failed
    /// tool observation (see [`Self::attach_failure_nudge`]), never pushed
    /// as a synthetic User message into canonical/DB.
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

    /// Append a failure-retry nudge onto the failed tool observation in a
    /// provider request buffer (Phase 7 / G5). Requires
    /// `failed_tool_call_id` so a parallel success that completes later cannot
    /// receive the nudge. Never invents a User row — if the id is missing or
    /// unmatched, the nudge is dropped rather than polluting the transcript.
    pub(super) fn attach_failure_nudge(
        messages: &mut [CanonicalMessage],
        nudge: &str,
        failed_tool_call_id: Option<&str>,
    ) {
        let Some(id) = failed_tool_call_id else {
            return;
        };
        let idx = messages
            .iter()
            .rev()
            .position(|m| m.role == CanonicalRole::Tool && m.tool_call_id.as_deref() == Some(id))
            .map(|rev_i| messages.len() - 1 - rev_i);
        let Some(idx) = idx else {
            return;
        };
        let msg = &mut messages[idx];
        if let Some(ContentPart::Text(text)) = msg.content.last_mut() {
            text.push_str("\n\n");
            text.push_str(nudge);
        } else {
            msg.content.push(ContentPart::text(nudge));
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

/// Result of one tool execution, kept in call order until the complete batch
/// is materialized into the canonical transcript. Tool execution may finish
/// in any order; the model must always receive tool observations in the same
/// order as the assistant's tool-call list.
struct CompletedTool {
    action: Action,
    tool_name: String,
    step_result: String,
    is_error: bool,
    ask_question: Option<String>,
    ask_options: Vec<String>,
    notify_title: Option<String>,
    notify_body: Option<String>,
    step_id: String,
    action_index: u32,
}

struct ToolBatchGate {
    all: Arc<RwLock<()>>,
    resources: AsyncMutex<HashMap<String, Arc<AsyncMutex<()>>>>,
}

#[allow(dead_code)]
enum ToolBatchPermit {
    Read(OwnedRwLockReadGuard<()>),
    Resource(OwnedMutexGuard<()>, OwnedRwLockReadGuard<()>),
    Exclusive(OwnedRwLockWriteGuard<()>),
}

impl ToolBatchGate {
    async fn acquire(&self, policy: &ToolConcurrency) -> ToolBatchPermit {
        match policy {
            ToolConcurrency::ReadOnly => ToolBatchPermit::Read(self.all.clone().read_owned().await),
            ToolConcurrency::Resource(key) => {
                let resource = {
                    let mut resources = self.resources.lock().await;
                    resources
                        .entry(key.clone())
                        .or_insert_with(|| Arc::new(AsyncMutex::new(())))
                        .clone()
                };
                ToolBatchPermit::Resource(
                    resource.lock_owned().await,
                    self.all.clone().read_owned().await,
                )
            }
            ToolConcurrency::Exclusive => {
                ToolBatchPermit::Exclusive(self.all.clone().write_owned().await)
            }
        }
    }
}

impl ReActEngine {
    /// Execute the non-final actions for one step: emit Action cards, run the
    /// batch (parallel), drain observations, failure nudge, and ask pause.
    /// Behavior-preserving extract from `run_react_loop` (Phase 1 / E2).
    ///
    /// Phase 7 / E5: tool-input validation runs at the tool-batch boundary
    /// before Action cards are emitted — not in the thin loop. Invalid inputs
    /// become failed observations and are never rewritten.
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn execute_tool_batch(
        &self,
        session_id: &str,
        state: &mut ReActState,
        step_num: u32,
        emitter: &Arc<dyn AgentEventEmitter>,
        run_id: u64,
        actions: &mut [Action],
        thought: &Option<String>,
        response: &haven_llm::LlmResponse,
        cancel_res: &tokio_util::sync::CancellationToken,
        max_steps: u32,
    ) -> anyhow::Result<ToolBatchOutcome> {
        let validation_failures = if !actions.is_empty() {
            self.validate_tool_inputs(session_id, actions).await
        } else {
            Vec::new()
        };
        if !validation_failures.is_empty() {
            tracing::warn!(
                "ReAct step {} session {} rejected {} invalid tool call(s)",
                step_num,
                session_id,
                validation_failures.len()
            );
        }
        let non_final: Vec<&Action> = actions.iter().filter(|a| !a.is_final).collect();
        // Mint one `step-*` id per action, shared by the Action event,
        // the tool's step row (created inside execute_step) and the
        // Observation event, so the live card and the resume badge (both
        // keyed `step-<id>`) are one entity. The ids are indexed by the
        // action's position in `non_final` (NOT by `tool_call_id`, which
        // two actions of a malformed provider response could share —
        // keying by it would collapse both onto one step id and the
        // second step-row insert would fail the PRIMARY KEY).
        let action_step_ids: Vec<String> = non_final
            .iter()
            .map(|_| haven_common::types::new_id("step"))
            .collect();

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
            let tool_calls: Vec<CanonicalToolCall> = non_final
                .iter()
                .map(|a| CanonicalToolCall {
                    id: a.tool_call_id.clone().unwrap_or_default(),
                    name: a.tool_name.clone(),
                    arguments: a.tool_input.clone(),
                })
                .collect();
            // Text matches Thought projection (trimmed) so review/resume
            // share one id/content; a retry-replaced response must not echo
            // the cut-off original text.
            // `parse_default_model_response` intentionally drops leaked
            // one-character tool-call fragments. Do not reintroduce the raw
            // response text when projecting the assistant/tool-call record.
            let push_text = thought.as_deref().unwrap_or("");
            let suppress_streamed_thought = thought.is_none() && !response.text.trim().is_empty();
            // A response mixing real tool calls with a web search round
            // carries both: the `web_search_call` items round-trip in the
            // same assistant message so the next request restores the
            // search context alongside the function tool results.
            // Phase 6.1 + X12: Action cards + pending rows + canonical via apply.
            // Thought already projected the messages row — no persist_text_id.
            let action_cards: Vec<ActionCard> = non_final
                .iter()
                .enumerate()
                .map(|(idx, action)| ActionCard {
                    tool_name: action.tool_name.clone(),
                    tool_input: action.tool_input.clone(),
                    tool_call_id: action.tool_call_id.clone(),
                    step_id: action_step_ids[idx].clone(),
                    action_index: idx as u32,
                    suppress_streamed_thought,
                })
                .collect();
            let step_ctx = StepCtx {
                session_id: session_id.to_string(),
                step_num,
                run_id,
                emitter: emitter.clone(),
            };
            self.apply_transcript(
                &step_ctx,
                TranscriptEvent::ToolCall {
                    text: push_text.to_string(),
                    tool_calls,
                    reasoning: if response.thinking_blocks.is_empty() {
                        response.reasoning.clone()
                    } else {
                        None
                    },
                    web_search_calls: response.web_search_calls.clone(),
                    thinking_blocks: response.thinking_blocks.clone(),
                    action_cards,
                    persist_text_id: None,
                },
                state,
            )
            .await;
        }

        self.save_branch_point(session_id, state, step_num, false)
            .await;

        use futures_util::StreamExt;

        // Phase 5 / E3: pre-check every non-final action before spawning.
        // Proceed tools run in parallel; blocked calls become immediate
        // results; NeedConfirm is collected and pauses after the drain.
        let gate_ctx = StepCtx {
            session_id: session_id.to_string(),
            step_num,
            run_id,
            emitter: emitter.clone(),
        };
        let mut need_confirm: Vec<ConfirmPendingTool> = Vec::new();
        let mut proceed: Vec<(usize, Action, Option<bool>, ToolConcurrency)> = Vec::new();
        let mut completed_results: Vec<Option<CompletedTool>> =
            (0..non_final.len()).map(|_| None).collect();
        // Accumulates result-derived control signals while keeping the
        // projection itself ordered by the assistant's tool-call list.
        let mut batch_state = ToolBatchState::default();

        for (idx, action) in non_final.iter().enumerate() {
            if idx >= MAX_RUNTIME_TOOL_CALLS_PER_BATCH {
                let step_id = action_step_ids[idx].clone();
                let error = format!(
                    "runtime tool-call limit ({MAX_RUNTIME_TOOL_CALLS_PER_BATCH}) exceeded; call was not executed"
                );
                self.executor
                    .finish_step_with_outcome(
                        session_id,
                        &action.tool_name,
                        &action.tool_input,
                        step_num,
                        idx as u32,
                        action.tool_call_id.as_deref(),
                        &step_id,
                        &error,
                        ActionStepOutcome::Failed,
                    )
                    .await;
                completed_results[idx] = Some(CompletedTool {
                    action: (*action).clone(),
                    tool_name: action.tool_name.clone(),
                    step_result: error,
                    is_error: true,
                    ask_question: None,
                    ask_options: Vec::new(),
                    notify_title: None,
                    notify_body: None,
                    step_id,
                    action_index: idx as u32,
                });
                continue;
            }
            if let Some(failure) = validation_failures
                .iter()
                .find(|failure| failure.action_index == idx as u32)
            {
                let error = failure.render();
                let step_id = action_step_ids[idx].clone();
                self.executor
                    .finish_interrupted_step_with_identity(
                        session_id,
                        &action.tool_name,
                        &action.tool_input,
                        step_num,
                        idx as u32,
                        action.tool_call_id.as_deref(),
                        &step_id,
                        &error,
                    )
                    .await;
                completed_results[idx] = Some(CompletedTool {
                    action: (*action).clone(),
                    tool_name: action.tool_name.clone(),
                    step_result: error,
                    is_error: true,
                    ask_question: None,
                    ask_options: Vec::new(),
                    notify_title: None,
                    notify_body: None,
                    step_id,
                    action_index: idx as u32,
                });
                continue;
            }
            match self
                .hooks
                .before_tool(
                    self,
                    &gate_ctx,
                    ToolCallIdentity {
                        step_id: &action_step_ids[idx],
                        action_index: idx as u32,
                        tool_call_id: action.tool_call_id.as_deref(),
                    },
                    &action.tool_name,
                    &action.tool_input,
                )
                .await
            {
                BeforeToolAction::Proceed { confirmed } => {
                    let concurrency = self
                        .executor
                        .tool_concurrency(session_id, &action.tool_name, &action.tool_input)
                        .await;
                    proceed.push((idx, (*action).clone(), confirmed, concurrency));
                }
                BeforeToolAction::Block { error } => {
                    let step_id = action_step_ids[idx].clone();
                    self.executor
                        .finish_interrupted_step_with_identity(
                            session_id,
                            &action.tool_name,
                            &action.tool_input,
                            step_num,
                            idx as u32,
                            action.tool_call_id.as_deref(),
                            &step_id,
                            &error,
                        )
                        .await;
                    completed_results[idx] = Some(CompletedTool {
                        action: (*action).clone(),
                        tool_name: action.tool_name.clone(),
                        step_result: error,
                        is_error: true,
                        ask_question: None,
                        ask_options: Vec::new(),
                        notify_title: None,
                        notify_body: None,
                        step_id,
                        action_index: idx as u32,
                    });
                }
                BeforeToolAction::NeedConfirm { risk_level } => {
                    need_confirm.push(ConfirmPendingTool {
                        confirm_id: haven_common::types::new_id("conf"),
                        tool_name: action.tool_name.clone(),
                        tool_input: action.tool_input.clone(),
                        tool_call_id: action.tool_call_id.clone().unwrap_or_default(),
                        step_id: action_step_ids[idx].clone(),
                        action_index: idx as u32,
                        risk_level,
                        decision: None,
                    });
                }
            }
        }

        let gate = Arc::new(ToolBatchGate {
            all: Arc::new(RwLock::new(())),
            resources: AsyncMutex::new(HashMap::new()),
        });
        let started = Arc::new(
            (0..non_final.len())
                .map(|_| AtomicBool::new(false))
                .collect::<Vec<_>>(),
        );
        let mut tool_futures = futures_util::stream::iter(proceed)
            .map(|(idx, action, confirmed, concurrency)| {
                let session_id = session_id.to_string();
                let tool_name = action.tool_name.clone();
                let tool_input = action.tool_input.clone();
                let executor = self.executor.clone();
                let gate = gate.clone();
                let started = started.clone();
                // The same step id minted at Action-emit time keys the step
                // row execute_step creates, so the live card id, the DB badge
                // id and this step id are identical everywhere.
                let step_id = action_step_ids[idx].clone();
                let pre_confirmed = confirmed == Some(true);
                async move {
                    let _permit = gate.acquire(&concurrency).await;
                    // The call is considered in-flight only after its
                    // resource permit is acquired. A future waiting behind a
                    // conflicting write can therefore be cancelled as
                    // `cancelled`, not conservatively misreported unknown.
                    started[idx].store(true, Ordering::Release);
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
                            .execute_step_preconfirmed_with_identity(
                                &session_id,
                                &tool_name,
                                tool_input.clone(),
                                step_num,
                                idx as u32,
                                action.tool_call_id.as_deref(),
                                &step_id,
                                true,
                            )
                            .await
                    } else {
                        executor
                            .execute_step_with_identity(
                                &session_id,
                                &tool_name,
                                tool_input.clone(),
                                step_num,
                                idx as u32,
                                action.tool_call_id.as_deref(),
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
                                let text = executor.observation_text(&tool_name, &r).await;
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
                        idx,
                        CompletedTool {
                            action,
                            tool_name,
                            step_result: text,
                            is_error,
                            ask_question,
                            ask_options,
                            notify_title,
                            notify_body,
                            step_id,
                            action_index: idx as u32,
                        },
                    )
                }
            })
            .buffer_unordered(MAX_CONCURRENT_TOOL_CALLS);

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
                        if completed_results[idx].is_some() {
                            continue;
                        }
                        let was_started = started[idx].load(Ordering::Acquire);
                        let interrupted_text = if was_started {
                            crate::canonical::interrupted_result_text(
                                &action.tool_name,
                                &action.tool_input,
                            )
                        } else {
                            "tool call cancelled before execution".to_string()
                        };
                        let outcome = if was_started {
                            ActionStepOutcome::Unknown
                        } else {
                            ActionStepOutcome::Cancelled
                        };
                        let step_id = action_step_ids[idx].clone();
                        self.executor
                            .finish_step_with_outcome(
                                session_id,
                                &action.tool_name,
                                &action.tool_input,
                                step_num,
                                idx as u32,
                                action.tool_call_id.as_deref(),
                                &step_id,
                                &interrupted_text,
                                outcome,
                            )
                            .await;
                        completed_results[idx] = Some(CompletedTool {
                            action: (*action).clone(),
                            tool_name: action.tool_name.clone(),
                            step_result: interrupted_text,
                            is_error: true,
                            ask_question: None,
                            ask_options: Vec::new(),
                            notify_title: None,
                            notify_body: None,
                            step_id,
                            action_index: idx as u32,
                        });
                    }
                    for result in completed_results.into_iter().flatten() {
                        batch_state
                            .commit_tool_result(self, &gate_ctx, result, state)
                            .await;
                    }
                    // A rollback that lands mid-batch must find the DB row
                    // at the pre-batch branch point (the response and
                    // partial tool results are discarded by the exit).
                    return Ok(ToolBatchOutcome::Done(
                        self.exit_cancelled(session_id, state, step_num)
                        .await,
                    ));
                }
                item = tool_futures.next() => {
                    let Some((idx, result)) = item else {
                        break;
                    };
                    completed_results[idx] = Some(result);
                }
            }
        }

        // Futures finish nondeterministically, but canonical tool messages are
        // an ordered protocol: each observation follows the corresponding
        // assistant call. Buffering only the projection keeps parallel tools
        // fast without making the next provider request depend on completion
        // order.
        for result in completed_results.into_iter().flatten() {
            batch_state
                .commit_tool_result(self, &gate_ctx, result, state)
                .await;
        }

        // Skip the retry nudge when the batch asked the user or is about to
        // pause for confirm: it would be baked into the paused snapshot ahead
        // of the user's real answer / decision. Phase 7 / G5: append onto the
        // last failed tool observation — never a synthetic User message.
        if batch_state.any_tool_failure
            && batch_state.asked_questions.is_empty()
            && need_confirm.is_empty()
            && step_num < max_steps - 1
        {
            let nudge = Self::build_failure_nudge(&batch_state.failure_signals);
            if let Some(tool_call_id) = batch_state.last_failed_tool_call_id.clone() {
                state.stage_retry_nudge(tool_call_id, nudge);
            }
        }

        // Phase 5 / E3: confirm before ask when both appear in one batch.
        // Ask pause used to return first and drop NeedConfirm tools (Action
        // cards + assistant tool_calls with no results → Interrupted repair).
        // Prefer confirm pause; stash ask pending so finish_confirm_batch's
        // next turn still surfaces the question.
        if !need_confirm.is_empty() {
            if !batch_state.asked_questions.is_empty() {
                // Ask question rows were projected inside apply(ToolResult).
                let question = batch_state.asked_questions.join("\n\n");
                self.executor
                    .set_awaiting_answer(
                        session_id,
                        Some(crate::types::AskPending {
                            question,
                            step_ids: batch_state.ask_step_ids.clone(),
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
            // UI-only waiting notice in `messages` (not an LLM event — must
            // not enter `react_state.events` or resume would re-feed it).
            let notice = "Waiting for confirmation…";
            self.project_chat_message(session_id, "assistant", notice, Some("text"), None, None)
                .await;
            self.pause_turn(PauseTurnInput {
                session_id,
                state,
                snapshot_step: step_num + 1,
                emitter,
                status: SessionStatus::PausedAwaitingConfirm,
                final_text: notice,
                branch_point_step: None,
            })
            .await?;
            return Ok(ToolBatchOutcome::Done(LoopExit::Paused {
                reason: PauseReason::Confirm,
            }));
        }

        // The agent asked the human a question: pause so the user can
        // answer. Their reply arrives as a supplement and resumes the session
        // (Paused —Pending —dispatcher re-enters the loop, injecting the
        // answer as context at the top of the next step).
        if !batch_state.asked_questions.is_empty() {
            let question = batch_state.asked_questions.join("\n\n");
            // X12: ask question messages were projected in apply(ToolResult)
            // under each ask step id (shared-id protocol).
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
                            step_ids: batch_state.ask_step_ids.clone(),
                        }),
                    )
                    .await;
                SessionStatus::PausedAwaitingAnswer
            };
            self.pause_turn(PauseTurnInput {
                session_id,
                state,
                snapshot_step: step_num + 1,
                emitter,
                status,
                final_text: &question,
                branch_point_step: None,
            })
            .await?;
            return Ok(ToolBatchOutcome::Done(LoopExit::Paused {
                reason: PauseReason::Ask,
            }));
        }

        let session_state = self.executor.get_session_state(session_id).await;
        match session_state {
            Some(s) if s.is_paused() => {
                return Ok(ToolBatchOutcome::Done(
                    self.exit_external_pause(session_id, state, step_num, emitter, run_id)
                        .await,
                ));
            }
            Some(SessionStatus::Error) => {
                return Ok(ToolBatchOutcome::Done(
                    self.exit_with_snapshot(
                        session_id,
                        state,
                        step_num,
                        LoopExit::Error("session interrupted".into()),
                    )
                    .await,
                ));
            }
            // Session gone (end_session/terminal cleanup) or completed: exit.
            None | Some(SessionStatus::Completed) => {
                return Ok(ToolBatchOutcome::Done(
                    self.exit_with_snapshot(session_id, state, step_num, LoopExit::Completed)
                        .await,
                ));
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
        state: &mut ReActState,
        emitter: &Arc<dyn AgentEventEmitter>,
        run_id: u64,
    ) -> anyhow::Result<ToolBatchOutcome> {
        let Some(pending) = self.executor.get_awaiting_confirm(session_id).await else {
            return Ok(ToolBatchOutcome::Continue);
        };
        let step_num = pending.step_number;
        let proj_ctx = StepCtx {
            session_id: session_id.to_string(),
            step_num,
            run_id,
            emitter: emitter.clone(),
        };
        let mut batch_state = ToolBatchState::default();

        for tool in pending.tools {
            let Some(decision) = tool.decision else {
                continue;
            };
            let tool_call_id = (!tool.tool_call_id.is_empty()).then(|| tool.tool_call_id.clone());
            let action = Action {
                tool_name: tool.tool_name.clone(),
                tool_input: tool.tool_input.clone(),
                is_final: false,
                tool_call_id: tool_call_id.clone(),
            };

            let result = if decision {
                let result = self
                    .executor
                    .execute_step_preconfirmed_with_identity(
                        session_id,
                        &tool.tool_name,
                        tool.tool_input.clone(),
                        step_num,
                        tool.action_index,
                        tool_call_id.as_deref(),
                        &tool.step_id,
                        true,
                    )
                    .await;
                let (text, is_error, ask_question, ask_options, notify_title, notify_body) =
                    match result {
                        Ok(r) => {
                            let text = self.executor.observation_text(&tool.tool_name, &r).await;
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
                CompletedTool {
                    action,
                    tool_name: tool.tool_name,
                    step_result: text,
                    is_error,
                    ask_question,
                    ask_options,
                    notify_title,
                    notify_body,
                    step_id: tool.step_id,
                    action_index: tool.action_index,
                }
            } else {
                let error = format!(
                    "The user REJECTED the operation '{}' (confirmation declined). Do NOT retry it — ask the user what to do instead or choose a different approach.",
                    tool.tool_name
                );
                self.executor
                    .finish_interrupted_step_with_identity(
                        session_id,
                        &tool.tool_name,
                        &tool.tool_input,
                        step_num,
                        tool.action_index,
                        tool_call_id.as_deref(),
                        &tool.step_id,
                        &error,
                    )
                    .await;
                CompletedTool {
                    action,
                    tool_name: tool.tool_name,
                    step_result: error,
                    is_error: true,
                    ask_question: None,
                    ask_options: Vec::new(),
                    notify_title: None,
                    notify_body: None,
                    step_id: tool.step_id,
                    action_index: tool.action_index,
                }
            };
            batch_state
                .commit_tool_result(self, &proj_ctx, result, state)
                .await;
        }

        self.executor
            .clear_awaiting_confirm_persisted(session_id)
            .await;

        if !batch_state.asked_questions.is_empty() {
            let question = batch_state.asked_questions.join("\n\n");
            self.executor
                .set_awaiting_answer(
                    session_id,
                    Some(crate::types::AskPending {
                        question,
                        step_ids: batch_state.ask_step_ids.clone(),
                    }),
                )
                .await;
        }

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
            self.pause_turn(PauseTurnInput {
                session_id,
                state,
                snapshot_step: step_num + 1,
                emitter,
                status,
                final_text: &ask.question,
                branch_point_step: None,
            })
            .await?;
            return Ok(ToolBatchOutcome::Done(LoopExit::Paused {
                reason: PauseReason::Ask,
            }));
        }

        Ok(ToolBatchOutcome::Continue)
    }
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
    fn is_agent_inbox_call_requires_name_and_operation() {
        assert!(is_agent_inbox_call(
            "agent",
            &serde_json::json!({"operation": "inbox"})
        ));
        assert!(!is_agent_inbox_call(
            "agent",
            &serde_json::json!({"operation": "list"})
        ));
        assert!(!is_agent_inbox_call(
            "message_inbox",
            &serde_json::json!({"operation": "inbox"})
        ));
        assert!(!is_agent_inbox_call("agent", &serde_json::json!({})));
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

    #[test]
    fn attach_failure_nudge_appends_to_failed_tool_not_user() {
        let mut canonical = vec![
            CanonicalMessage::user_text("please help"),
            CanonicalMessage::assistant(
                vec![ContentPart::text("")],
                None,
                None,
                Vec::new(),
                Vec::new(),
            ),
            CanonicalMessage::tool(vec![ContentPart::text("ok")], Some("call-ok".into())),
            CanonicalMessage::tool(
                vec![ContentPart::text("curl: connection refused")],
                Some("call-fail".into()),
            ),
        ];
        let nudge = "The tool failures look ENVIRONMENTAL";
        ReActEngine::attach_failure_nudge(&mut canonical, nudge, Some("call-fail"));

        assert_eq!(canonical.len(), 4, "must not invent a new message");
        assert!(
            canonical
                .iter()
                .filter(|m| m.role == CanonicalRole::User)
                .all(|m| {
                    !m.content.iter().any(|p| match p {
                        ContentPart::Text(t) => t.contains("ENVIRONMENTAL"),
                        _ => false,
                    })
                }),
            "nudge must not appear on a User message"
        );
        let failed = canonical
            .iter()
            .find(|m| m.tool_call_id.as_deref() == Some("call-fail"))
            .expect("failed tool message");
        let ContentPart::Text(text) = &failed.content[0] else {
            panic!("expected text content");
        };
        assert!(
            text.contains("curl: connection refused") && text.contains("ENVIRONMENTAL"),
            "nudge should append onto the failed tool observation, got: {text}"
        );
        let ok = canonical
            .iter()
            .find(|m| m.tool_call_id.as_deref() == Some("call-ok"))
            .expect("ok tool message");
        let ContentPart::Text(ok_text) = &ok.content[0] else {
            panic!("expected text content");
        };
        assert_eq!(
            ok_text, "ok",
            "successful tool observation must stay untouched"
        );
    }

    #[test]
    fn attach_failure_nudge_noop_without_failed_id() {
        let mut canonical = vec![CanonicalMessage::tool(
            vec![ContentPart::text("boom")],
            Some("call-1".into()),
        )];
        ReActEngine::attach_failure_nudge(&mut canonical, "retry hint", None);
        let ContentPart::Text(text) = &canonical[0].content[0] else {
            panic!("expected text");
        };
        assert_eq!(text, "boom", "must not attach without failed tool_call_id");
    }

    #[test]
    fn attach_failure_nudge_noop_on_unmatched_id() {
        let mut canonical = vec![CanonicalMessage::tool(
            vec![ContentPart::text("ok")],
            Some("call-ok".into()),
        )];
        ReActEngine::attach_failure_nudge(&mut canonical, "retry hint", Some("call-miss"));
        let ContentPart::Text(text) = &canonical[0].content[0] else {
            panic!("expected text");
        };
        assert_eq!(text, "ok");
    }

    #[test]
    fn attach_failure_nudge_noop_without_tool_message() {
        let mut canonical = vec![CanonicalMessage::user_text("hello")];
        ReActEngine::attach_failure_nudge(&mut canonical, "should not appear", Some("call-x"));
        assert_eq!(canonical.len(), 1);
        let ContentPart::Text(text) = &canonical[0].content[0] else {
            panic!("expected text");
        };
        assert_eq!(text, "hello");
    }

    #[tokio::test]
    async fn tool_batch_gate_serializes_same_resource_and_allows_reads() {
        let gate = Arc::new(ToolBatchGate {
            all: Arc::new(RwLock::new(())),
            resources: AsyncMutex::new(HashMap::new()),
        });
        let first = gate
            .acquire(&ToolConcurrency::Resource("file-a".into()))
            .await;
        let waiting = tokio::time::timeout(
            std::time::Duration::from_millis(20),
            gate.acquire(&ToolConcurrency::Resource("file-a".into())),
        )
        .await;
        assert!(waiting.is_err(), "same resource writes must serialize");
        drop(first);

        let read = gate.acquire(&ToolConcurrency::ReadOnly).await;
        let second_read = tokio::time::timeout(
            std::time::Duration::from_millis(20),
            gate.acquire(&ToolConcurrency::ReadOnly),
        )
        .await;
        assert!(second_read.is_ok(), "read-only calls may overlap");
        drop(read);
    }

    #[test]
    fn runtime_tool_call_limit_is_bounded() {
        assert!(MAX_RUNTIME_TOOL_CALLS_PER_BATCH > 0);
        assert!(MAX_RUNTIME_TOOL_CALLS_PER_BATCH < usize::MAX);
    }
}
