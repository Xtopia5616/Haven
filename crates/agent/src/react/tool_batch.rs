//! Tool-batch state, safety admission, confirmation, and ordered materialization.
//!
//! Tool execution is concurrent, but transcript materialization is ordered by
//! the assistant's tool-call list so the next model request is deterministic.

use super::snapshot_io::PauseTurnInput;
#[cfg(test)]
use super::tool_batch_policy::FailureKind;
use super::tool_batch_policy::{empty_inbox_output, is_agent_inbox_call};
use super::*;
use crate::types::Action;
#[cfg(test)]
use haven_common::types::{CanonicalMessage, CanonicalRole, ContentPart};
use haven_memory::repositories::session_steps::ActionStepOutcome;
use haven_tools::{ToolConcurrency, is_silent_action};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{Mutex as AsyncMutex, OwnedRwLockReadGuard, OwnedRwLockWriteGuard, RwLock};

/// Hard safety ceilings for one assistant response. The first bounds total
/// work admitted to the runtime; the second bounds live futures/tasks. The
/// provider-facing tool-definition limit is not a runtime execution limit.
pub(crate) const MAX_RUNTIME_TOOL_CALLS_PER_BATCH: usize = 64;
pub(super) const MAX_CONCURRENT_TOOL_CALLS: usize = 8;

#[derive(Default)]
pub(super) struct ToolBatchState {
    pub(super) any_tool_failure: bool,
    pub(super) failure_signals: Vec<(String, String)>,
    pub(super) last_failed_tool_call_id: Option<String>,
    pub(super) asked_questions: Vec<String>,
    pub(super) ask_step_ids: Vec<String>,
}

impl ToolBatchState {
    pub(super) async fn commit_tool_result(
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
pub(super) struct CompletedTool {
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

impl CompletedTool {
    pub(super) fn failed(
        action: Action,
        step_id: String,
        action_index: u32,
        step_result: String,
    ) -> Self {
        Self {
            tool_name: action.tool_name.clone(),
            action,
            step_result,
            is_error: true,
            ask_question: None,
            ask_options: Vec::new(),
            notify_title: None,
            notify_body: None,
            step_id,
            action_index,
        }
    }
}

/// Execute one already-admitted tool call and normalize its result into the
/// batch representation. Both the normal batch and the post-confirm resume
/// path use this helper so observation truncation and tool-owned signals
/// cannot drift between the two paths.
pub(super) async fn execute_tool_action(
    executor: Arc<SessionExecutor>,
    session_id: String,
    action: Action,
    step_num: u32,
    action_index: u32,
    step_id: String,
    pre_confirmed: bool,
) -> CompletedTool {
    let tool_name = action.tool_name.clone();
    let tool_input = action.tool_input.clone();
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
                tool_input,
                step_num,
                action_index,
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
                tool_input,
                step_num,
                action_index,
                action.tool_call_id.as_deref(),
                &step_id,
            )
            .await
    };

    let (step_result, is_error, ask_question, ask_options, notify_title, notify_body) = match result
    {
        Ok(result) => {
            tracing::debug!(
                "tool '{}' at step {} completed: success={}, {} chars",
                tool_name,
                step_num,
                result.success,
                serde_json::to_string(&result.output)
                    .map(|text| text.len())
                    .unwrap_or(0)
            );
            tracing::trace!(
                "tool '{}' at step {} full output: {} chars",
                tool_name,
                step_num,
                serde_json::to_string(&result.output)
                    .map(|text| text.len())
                    .unwrap_or(0)
            );
            let step_result = executor.observation_text(&tool_name, &result).await;
            (
                step_result,
                !result.success,
                result.signals.ask_question,
                result.signals.ask_options,
                result.signals.notify_title,
                result.signals.notify_body,
            )
        }
        Err(error) => {
            tracing::debug!(
                "tool '{}' at step {} failed: {}",
                tool_name,
                step_num,
                error
            );
            (error.to_string(), true, None, Vec::new(), None, None)
        }
    };

    CompletedTool {
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
    }
}

pub(super) struct ToolBatchGate {
    pub(super) all: Arc<RwLock<()>>,
    pub(super) resources: AsyncMutex<HashMap<String, Arc<RwLock<()>>>>,
}

#[allow(dead_code)]
pub(super) enum ToolBatchPermit {
    Read(OwnedRwLockReadGuard<()>),
    SharedResource(OwnedRwLockReadGuard<()>, OwnedRwLockReadGuard<()>),
    Resource(OwnedRwLockWriteGuard<()>, OwnedRwLockReadGuard<()>),
    Exclusive(OwnedRwLockWriteGuard<()>),
}

impl ToolBatchGate {
    pub(super) async fn acquire(&self, policy: &ToolConcurrency) -> ToolBatchPermit {
        match policy {
            ToolConcurrency::ReadOnly => ToolBatchPermit::Read(self.all.clone().read_owned().await),
            ToolConcurrency::SharedResource(key) | ToolConcurrency::Resource(key) => {
                let resource = {
                    let mut resources = self.resources.lock().await;
                    resources
                        .entry(key.clone())
                        .or_insert_with(|| Arc::new(RwLock::new(())))
                        .clone()
                };
                let batch_read = self.all.clone().read_owned().await;
                match policy {
                    ToolConcurrency::SharedResource(_) => {
                        ToolBatchPermit::SharedResource(resource.read_owned().await, batch_read)
                    }
                    ToolConcurrency::Resource(_) => {
                        ToolBatchPermit::Resource(resource.write_owned().await, batch_read)
                    }
                    _ => unreachable!("resource branch only matches resource policies"),
                }
            }
            ToolConcurrency::Exclusive => {
                ToolBatchPermit::Exclusive(self.all.clone().write_owned().await)
            }
        }
    }
}

impl ReActEngine {
    /// Apply the shared post-ask transition. A reply that arrived while the
    /// batch was running turns the session back into `Pending`; otherwise the
    /// explicit ask gate is persisted before pausing. Keeping this transition
    /// here makes normal execution and confirm-resume agree on queue and
    /// snapshot semantics.
    pub(super) async fn pause_for_ask(
        &self,
        session_id: &str,
        state: &mut ReActState,
        step_num: u32,
        emitter: &Arc<dyn AgentEventEmitter>,
        pending: crate::types::AskPending,
    ) -> anyhow::Result<ToolBatchOutcome> {
        self.executor.mark_user_queues_as_answer(session_id).await;
        let has_answer = self.executor.has_pending_context(session_id).await;
        let status = if has_answer {
            self.executor.clear_awaiting_answer(session_id).await;
            SessionStatus::Pending
        } else {
            self.executor
                .set_awaiting_answer(session_id, Some(pending.clone()))
                .await;
            SessionStatus::PausedAwaitingAnswer
        };
        self.pause_turn(PauseTurnInput {
            session_id,
            state,
            snapshot_step: step_num + 1,
            emitter,
            status,
            final_text: &pending.question,
            branch_point_step: None,
        })
        .await?;
        Ok(ToolBatchOutcome::Done(LoopExit::Paused {
            reason: PauseReason::Ask,
        }))
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
                execute_tool_action(
                    self.executor.clone(),
                    session_id.to_string(),
                    action,
                    step_num,
                    tool.action_index,
                    tool.step_id,
                    true,
                )
                .await
            } else {
                let error = format!(
                    "The user REJECTED the operation '{}' (confirmation declined). Do NOT retry it — ask the user what to do instead or choose a different approach.",
                    tool.tool_name
                );
                self.executor
                    .finish_step_with_outcome(
                        session_id,
                        &tool.tool_name,
                        &tool.tool_input,
                        step_num,
                        tool.action_index,
                        tool_call_id.as_deref(),
                        &tool.step_id,
                        &error,
                        ActionStepOutcome::Cancelled,
                    )
                    .await;
                CompletedTool::failed(action, tool.step_id, tool.action_index, error)
            };
            batch_state
                .commit_tool_result(self, &proj_ctx, result, state)
                .await;
        }

        self.executor
            .clear_awaiting_confirm_persisted(session_id)
            .await;

        let pending_ask = if !batch_state.asked_questions.is_empty() {
            Some(crate::types::AskPending {
                question: batch_state.asked_questions.join("\n\n"),
                step_ids: batch_state.ask_step_ids.clone(),
            })
        } else {
            // Same-batch ask was stashed while confirm paused first: surface
            // it now.
            self.executor.get_awaiting_answer(session_id).await
        };
        if let Some(pending) = pending_ask {
            return self
                .pause_for_ask(session_id, state, step_num, emitter, pending)
                .await;
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

        let blocked_reader = tokio::time::timeout(
            std::time::Duration::from_millis(20),
            gate.acquire(&ToolConcurrency::SharedResource("file-a".into())),
        )
        .await;
        assert!(
            blocked_reader.is_err(),
            "resource readers must not overlap a resource write"
        );
        drop(first);

        let read = gate
            .acquire(&ToolConcurrency::SharedResource("file-a".into()))
            .await;
        let second_read = tokio::time::timeout(
            std::time::Duration::from_millis(20),
            gate.acquire(&ToolConcurrency::SharedResource("file-a".into())),
        )
        .await;
        assert!(second_read.is_ok(), "resource readers may overlap");
        let blocked_writer = tokio::time::timeout(
            std::time::Duration::from_millis(20),
            gate.acquire(&ToolConcurrency::Resource("file-a".into())),
        )
        .await;
        assert!(
            blocked_writer.is_err(),
            "resource writes must wait for resource readers"
        );
        drop(read);

        let read_only = gate.acquire(&ToolConcurrency::ReadOnly).await;
        drop(read_only);
    }

    #[test]
    fn runtime_tool_call_limit_is_bounded() {
        assert!(MAX_RUNTIME_TOOL_CALLS_PER_BATCH > 0);
        assert!(MAX_RUNTIME_TOOL_CALLS_PER_BATCH < usize::MAX);
    }
}
