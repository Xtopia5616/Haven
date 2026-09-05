//! Tool-batch result state, observation normalization, and ordered materialization.
//!
//! Admission/execution lives in `tool_batch_execute`; transcript materialization
//! remains ordered by the assistant's tool-call list so the next model request
//! is deterministic.

use super::snapshot_io::PauseTurnInput;
#[cfg(test)]
use super::tool_batch_policy::FailureKind;
use super::tool_batch_policy::{
    empty_inbox_output, is_agent_inbox_call, is_retryable_failure_outcome,
};
use super::*;
use crate::session::ActionStepPersistenceError;
use crate::types::Action;
#[cfg(test)]
use haven_common::types::{CanonicalMessage, CanonicalRole, ContentPart};
use haven_tools::{ToolConcurrency, ToolExecutionOutcome, is_silent_action};
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
    pub(super) retryable_failure: bool,
    pub(super) failure_signals: Vec<(String, String)>,
    pub(super) last_retryable_failed_tool_call_id: Option<String>,
    pub(super) asked_questions: Vec<String>,
    pub(super) ask_step_ids: Vec<String>,
}

impl ToolBatchState {
    /// Commit observations in plan order, regardless of the order in which
    /// the executor completed them. The result slots are indexed by the
    /// `ToolBatchPlan`, so callers cannot accidentally make provider history
    /// depend on runtime completion order.
    pub(super) async fn commit_ordered_results(
        &mut self,
        engine: &ReActEngine,
        ctx: &StepCtx,
        results: ToolBatchResults,
        state: &mut ReActState,
    ) -> anyhow::Result<()> {
        for result in results.into_ordered() {
            self.commit_tool_result(engine, ctx, result, state).await?;
        }
        Ok(())
    }

    pub(super) async fn commit_tool_result(
        &mut self,
        engine: &ReActEngine,
        ctx: &StepCtx,
        result: CompletedTool,
        state: &mut ReActState,
    ) -> anyhow::Result<()> {
        let CompletedTool {
            action,
            tool_name,
            step_result,
            is_error,
            outcome,
            ask_question,
            ask_options,
            notify_title,
            notify_body,
            step_id,
            action_index,
        } = result;

        if is_error && is_retryable_failure_outcome(outcome) {
            self.retryable_failure = true;
            self.last_retryable_failed_tool_call_id = action.tool_call_id.clone();
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
            .await?;
        Ok(())
    }
}

/// Completion slots for one `ToolBatchPlan`. Execution writes by plan index;
/// projection consumes from index zero upward. This keeps the single-tool
/// path on the exact same semantic pipeline as larger batches without
/// allocating a second action/step-id index map.
pub(super) struct ToolBatchResults {
    slots: Vec<Option<CompletedTool>>,
}

impl ToolBatchResults {
    pub(super) fn new(len: usize) -> Self {
        Self {
            slots: (0..len).map(|_| None).collect(),
        }
    }

    pub(super) fn set(&mut self, index: usize, result: CompletedTool) {
        debug_assert!(index < self.slots.len(), "tool result index out of bounds");
        if let Some(slot) = self.slots.get_mut(index) {
            debug_assert!(slot.is_none(), "tool result slot completed twice");
            *slot = Some(result);
        }
    }

    pub(super) fn is_set(&self, index: usize) -> bool {
        self.slots.get(index).is_some_and(Option::is_some)
    }

    pub(super) fn is_complete(&self) -> bool {
        self.slots.iter().all(Option::is_some)
    }

    #[cfg(test)]
    pub(super) fn ready_prefix_len(&self) -> usize {
        self.slots.iter().take_while(|slot| slot.is_some()).count()
    }

    pub(super) fn into_ordered(self) -> Vec<CompletedTool> {
        assert!(
            self.is_complete(),
            "tool batch result slots must be complete before materialization"
        );
        self.slots
            .into_iter()
            .map(|slot| slot.expect("complete tool result slot"))
            .collect()
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
    pub(super) outcome: ToolExecutionOutcome,
    ask_question: Option<String>,
    ask_options: Vec<String>,
    notify_title: Option<String>,
    notify_body: Option<String>,
    step_id: String,
    action_index: u32,
}

impl CompletedTool {
    pub(super) fn from_observation(
        action: Action,
        step_id: String,
        action_index: u32,
        step_result: String,
        outcome: ToolExecutionOutcome,
    ) -> Self {
        Self {
            tool_name: action.tool_name.clone(),
            action,
            step_result,
            is_error: !matches!(outcome, ToolExecutionOutcome::Succeeded),
            outcome,
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

    let (step_result, is_error, outcome, ask_question, ask_options, notify_title, notify_body) =
        match result {
            Ok(result) => {
                let output_len = serde_json::to_string(&result.output)
                    .map(|text| text.len())
                    .unwrap_or(0);
                tracing::debug!(
                    "tool '{}' at step {} completed: success={}, {} chars",
                    tool_name,
                    step_num,
                    result.success,
                    output_len
                );
                tracing::trace!(
                    "tool '{}' at step {} full output: {} chars",
                    tool_name,
                    step_num,
                    output_len
                );
                let step_result = executor.observation_text(&tool_name, &result).await;
                (
                    step_result,
                    !result.success,
                    result.outcome,
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
                (
                    error.to_string(),
                    true,
                    if error.downcast_ref::<ActionStepPersistenceError>().is_some() {
                        ToolExecutionOutcome::TimedOutUnknown
                    } else {
                        ToolExecutionOutcome::Failed
                    },
                    None,
                    Vec::new(),
                    None,
                    None,
                )
            }
        };

    CompletedTool {
        action,
        tool_name,
        step_result,
        is_error,
        outcome,
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
    fn result_slots_commit_in_plan_order_after_out_of_order_completion() {
        let mut results = ToolBatchResults::new(2);
        results.set(
            1,
            CompletedTool::from_observation(
                Action {
                    tool_name: "second".into(),
                    tool_input: serde_json::json!({}),
                    is_final: false,
                    tool_call_id: Some("call-second".into()),
                },
                "step-second".into(),
                1,
                "second completed first".into(),
                haven_tools::ToolExecutionOutcome::Failed,
            ),
        );
        results.set(
            0,
            CompletedTool::from_observation(
                Action {
                    tool_name: "first".into(),
                    tool_input: serde_json::json!({}),
                    is_final: false,
                    tool_call_id: Some("call-first".into()),
                },
                "step-first".into(),
                0,
                "first completed second".into(),
                haven_tools::ToolExecutionOutcome::Failed,
            ),
        );

        let ordered: Vec<_> = results
            .into_ordered()
            .into_iter()
            .map(|result| result.action.tool_name)
            .collect();
        assert_eq!(ordered, ["first", "second"]);
    }

    #[test]
    fn result_slots_stop_at_unresolved_confirmation_barrier() {
        let mut results = ToolBatchResults::new(3);
        results.set(
            0,
            CompletedTool::from_observation(
                Action {
                    tool_name: "first".into(),
                    tool_input: serde_json::json!({}),
                    is_final: false,
                    tool_call_id: Some("call-first".into()),
                },
                "step-first".into(),
                0,
                "first".into(),
                haven_tools::ToolExecutionOutcome::Succeeded,
            ),
        );
        results.set(
            2,
            CompletedTool::from_observation(
                Action {
                    tool_name: "third".into(),
                    tool_input: serde_json::json!({}),
                    is_final: false,
                    tool_call_id: Some("call-third".into()),
                },
                "step-third".into(),
                2,
                "third".into(),
                haven_tools::ToolExecutionOutcome::Succeeded,
            ),
        );

        assert_eq!(results.ready_prefix_len(), 1);
        assert!(!results.is_complete());
    }

    #[test]
    fn completed_tool_preserves_unknown_execution_outcome() {
        let result = CompletedTool::from_observation(
            Action {
                tool_name: "send".into(),
                tool_input: serde_json::json!({}),
                is_final: false,
                tool_call_id: Some("call-send".into()),
            },
            "step-send".into(),
            0,
            "timed out".into(),
            haven_tools::ToolExecutionOutcome::TimedOutUnknown,
        );

        assert_eq!(
            result.outcome,
            haven_tools::ToolExecutionOutcome::TimedOutUnknown
        );
    }

    #[test]
    fn runtime_tool_call_limit_is_bounded() {
        assert!(MAX_RUNTIME_TOOL_CALLS_PER_BATCH > 0);
        assert!(MAX_RUNTIME_TOOL_CALLS_PER_BATCH < usize::MAX);
    }
}
