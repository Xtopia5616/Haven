//! Append-only transcript events and a unified `apply` (Phase 6 / B1-2 + H1;
//! Phase 8 / B1-3: the event store is the durable authority and canonical is
//! a projection cache updated here).
//!
//! # X12 projection contract
//!
//! The durable `session_events` rows are the append-only authority.
//! [`ReActEngine::apply_transcript`] submits a [`SessionCommitted`] domain
//! intent to `SessionStore`, which appends its events before materializing the
//! included `messages` / `session_steps` projections in one SQLite transaction.
//! A failure rolls back both; the store broadcasts only after commit.
//!
//! The committed rows are then published by
//! [`super::committed_ui::CommittedUiPublisher`] in sequence order, before
//! Agent updates in-memory canonical state. The assistant Thought message and
//! its shared-id thought step are both included in the commit transaction.
//!
//! Exceptions (documented, not parallel authorities):
//! - **Ingress user seed**: `layer`/`ingress` may insert the user `messages`
//!   row before `UserInject` is applied (crash-safe queue). `UserInject` with
//!   `message_id` assumes that row already exists and only creates the
//!   shared-id thought step.
//! - **Error partials**: `persist_partial_on_error` writes recovery-only rows
//!   intentionally *outside* the event log so continue/rollback can truncate
//!   them via `last_msg_at` without replaying a failed step.
//! - **Terminal ToolRun-result**: no live loop left — history-only persist.
//! - Interaction lifecycle and waiting state are not transcript content. Ask
//!   text exists once in the transcript; confirmation details live in the
//!   renderer-safe interaction projection.

use super::committed_ui::{
    CommittedUi, StoredObservationUi, StoredToolCallUi, encode_transcript_payload,
};
use super::*;
use crate::types::{ToolCall, TranscriptRecord, canonical_for_snapshot_with_media_inputs};
use haven_common::types::InjectSource;
use haven_common::types::{CanonicalToolCall, MessageAttachment};
use haven_memory::{CURRENT_EVENT_VERSION, SessionCommitted, SessionEvent, TRANSCRIPT_EVENT_TYPE};
use haven_tools::{
    OperationIdempotency, ToolExecutionOutcome, ToolOperationScope, ToolResultEnvelope,
};
use serde_json::Value;
use std::sync::Arc;

struct TranscriptProjection {
    record: TranscriptRecord,
    persisted_media_record: Option<TranscriptRecord>,
}

/// Pending ToolCall card (+ step row) emitted from [`TranscriptEvent::ToolCall`].
#[derive(Debug, Clone)]
pub(super) struct ToolCallCard {
    pub tool_name: String,
    pub tool_input: Value,
    pub tool_call_id: Option<String>,
    pub step_id: String,
    pub tool_index: u32,
    pub suppress_streamed_thought: bool,
    /// Resolved from the turn's immutable tool catalog snapshot. Keeping the
    /// result on the card prevents the transcript projection from reopening
    /// the live registry for every action in the batch.
    pub is_high_risk: bool,
    pub silent: bool,
}

/// Observation card emitted from [`TranscriptEvent::ToolResult`].
/// Display text is [`TranscriptEvent::ToolResult::history_observation`].
#[derive(Debug, Clone)]
pub(super) struct ObservationCard {
    pub tool_name: String,
    pub tool_call_id: Option<String>,
    pub step_id: String,
    pub tool_index: u32,
    pub silent: bool,
    pub ask_options: Vec<String>,
    pub outcome: ToolExecutionOutcome,
    pub idempotency: OperationIdempotency,
    pub operation_scope: ToolOperationScope,
    pub renderer: String,
    pub result_envelope: ToolResultEnvelope,
}

/// Runtime transcript event (UI cards + serializable payload).
/// Converted to [`TranscriptRecord`] when appended to the event log.
#[derive(Debug, Clone)]
pub(super) enum TranscriptEvent {
    Thought {
        text: String,
        message_id: String,
    },
    /// Reasoning block → `messages` projection (type=`reasoning`) + event log.
    Reasoning {
        text: String,
        message_id: String,
    },
    ToolCall {
        text: String,
        tool_calls: Vec<CanonicalToolCall>,
        reasoning: Option<String>,
        web_search_calls: Vec<serde_json::Value>,
        thinking_blocks: Vec<serde_json::Value>,
        tool_call_cards: Vec<ToolCallCard>,
        /// When `Some`, project `text` into `messages` under this id (final
        /// answer / synthetic text when Thought did not already project).
        persist_text_id: Option<String>,
    },
    ToolResult {
        canonical_observation: String,
        history_observation: String,
        tool_call_id: Option<String>,
        tool_call: ToolCall,
        tool_index: u32,
        step_id: String,
        observation_card: Option<Box<ObservationCard>>,
    },
    UserInject {
        source: InjectSource,
        text: String,
        attachments: Vec<MessageAttachment>,
        message_id: Option<String>,
    },
    CompactSummary {
        compacted: Vec<CanonicalMessage>,
        media_inputs: Vec<haven_common::media::MediaInput>,
        summary: String,
        tokens_before: u32,
        tokens_after: u32,
        episode_id: String,
        degraded: bool,
    },
}

impl TranscriptEvent {
    fn to_record(&self, step_number: u32) -> TranscriptRecord {
        match self {
            Self::Thought { text, message_id } => TranscriptRecord::Thought {
                step_number,
                text: text.clone(),
                message_id: message_id.clone(),
            },
            Self::Reasoning { text, message_id } => TranscriptRecord::Reasoning {
                step_number,
                text: text.clone(),
                message_id: message_id.clone(),
            },
            Self::ToolCall {
                text,
                tool_calls,
                reasoning,
                web_search_calls,
                thinking_blocks,
                ..
            } => TranscriptRecord::ToolCall {
                step_number,
                text: text.clone(),
                tool_calls: tool_calls.clone(),
                reasoning: reasoning.clone(),
                web_search_calls: web_search_calls.clone(),
                thinking_blocks: thinking_blocks.clone(),
            },
            Self::ToolResult {
                canonical_observation,
                history_observation,
                tool_call_id,
                tool_call,
                tool_index,
                step_id,
                ..
            } => TranscriptRecord::ToolResult {
                step_number,
                tool_index: *tool_index,
                step_id: step_id.clone(),
                canonical_observation: canonical_observation.clone(),
                history_observation: history_observation.clone(),
                tool_call_id: tool_call_id.clone(),
                tool_call: tool_call.clone(),
            },
            Self::UserInject {
                source,
                text,
                attachments,
                message_id,
            } => TranscriptRecord::UserInject {
                step_number,
                source: *source,
                text: text.clone(),
                media_inputs: attachments
                    .iter()
                    .map(haven_common::media::message_attachment_to_media_input)
                    .map(|input| input.for_snapshot())
                    .collect(),
                message_id: message_id.clone(),
            },
            Self::CompactSummary {
                compacted,
                media_inputs,
                summary,
                tokens_before,
                tokens_after,
                episode_id,
                degraded,
            } => TranscriptRecord::CompactSummary {
                step_number,
                compacted: canonical_for_snapshot_with_media_inputs(compacted, media_inputs),
                media_inputs: media_inputs
                    .iter()
                    .map(haven_common::media::MediaInput::for_snapshot)
                    .collect(),
                summary: summary.clone(),
                tokens_before: *tokens_before,
                tokens_after: *tokens_after,
                episode_id: episode_id.clone(),
                degraded: *degraded,
            },
        }
    }
}

fn normalize_transcript_event(event: TranscriptEvent) -> anyhow::Result<TranscriptEvent> {
    match event {
        TranscriptEvent::UserInject {
            source: InjectSource::ToolRunResult,
            text: _,
            attachments: _,
            message_id: None,
        } => anyhow::bail!("ToolRunResult transcript injection requires a stable message_id"),
        event => Ok(event),
    }
}

fn media_record_for_inject(
    step_number: u32,
    attachments: &[MessageAttachment],
    strategy: haven_common::media::MediaInputStrategy,
) -> Option<TranscriptRecord> {
    let media_inputs = attachments
        .iter()
        .map(haven_common::media::message_attachment_to_media_input)
        .collect::<Vec<_>>();
    let media_plan = crate::react::media_plan_for_inputs(&media_inputs, strategy);
    if media_plan.is_empty() && media_plan.notices.is_empty() {
        return None;
    }
    Some(TranscriptRecord::MediaPlan {
        step_number,
        strategy,
        media_inputs: media_inputs
            .iter()
            .map(haven_common::media::MediaInput::for_snapshot)
            .collect(),
        projections: media_plan.projections,
        notices: media_plan.notices,
    })
}

fn committed_ui_for(event: &TranscriptEvent) -> Option<CommittedUi> {
    match event {
        TranscriptEvent::ToolCall {
            tool_call_cards, ..
        } if !tool_call_cards.is_empty() => Some(CommittedUi::ToolCalls {
            cards: tool_call_cards
                .iter()
                .map(|card| StoredToolCallUi {
                    tool_name: card.tool_name.clone(),
                    tool_input: card.tool_input.clone(),
                    tool_call_id: card.tool_call_id.clone(),
                    step_id: card.step_id.clone(),
                    tool_index: card.tool_index,
                    suppress_streamed_thought: card.suppress_streamed_thought,
                })
                .collect(),
        }),
        TranscriptEvent::ToolResult {
            observation_card: Some(card),
            ..
        } => Some(CommittedUi::Observation {
            card: Box::new(StoredObservationUi {
                tool_name: card.tool_name.clone(),
                tool_call_id: card.tool_call_id.clone(),
                step_id: card.step_id.clone(),
                tool_index: card.tool_index,
                silent: card.silent,
                ask_options: card.ask_options.clone(),
                outcome: card.outcome.as_str().to_owned(),
                idempotency: card.idempotency.as_str().to_owned(),
                operation_scope: card.operation_scope.as_str().to_owned(),
                renderer: card.renderer.clone(),
                result: card.result_envelope.clone(),
            }),
        }),
        TranscriptEvent::UserInject { message_id, .. } => Some(CommittedUi::Supplement {
            supplement_id: message_id
                .clone()
                .unwrap_or_else(|| haven_common::types::new_id("msg")),
        }),
        _ => None,
    }
}

impl ReActEngine {
    async fn build_transcript_item(
        &self,
        ctx: &StepCtx,
        event: TranscriptEvent,
    ) -> anyhow::Result<(
        TranscriptEvent,
        TranscriptRecord,
        Option<TranscriptRecord>,
        SessionCommitted,
    )> {
        let event = normalize_transcript_event(event)?;
        let record = event.to_record(ctx.step_num);
        let mut committed = self.build_session_commit(ctx, &event, &record).await?;
        if let Some(ui) = committed_ui_for(&event)
            && let Some(first) = committed.events.first_mut()
        {
            first.payload = encode_transcript_payload(&record, Some(&ui))?;
        }
        let media_record = match &event {
            TranscriptEvent::UserInject { attachments, .. } => {
                media_record_for_inject(ctx.step_num, attachments, self.media_strategy())
            }
            _ => None,
        };
        if let Some(media_record) = &media_record {
            committed.push_transcript(
                serde_json::to_string(media_record)?,
                ctx.run_id,
                ctx.step_num,
            );
        }
        Ok((event, record, media_record, committed))
    }

    async fn build_session_commit(
        &self,
        ctx: &StepCtx,
        event: &TranscriptEvent,
        record: &TranscriptRecord,
    ) -> anyhow::Result<SessionCommitted> {
        let mut committed =
            SessionCommitted::transcript(serde_json::to_string(record)?, ctx.run_id, ctx.step_num);
        match event {
            TranscriptEvent::Thought { text, message_id } => {
                committed.project_thought_step(message_id.clone(), ctx.step_num);
                if !text.trim().is_empty() {
                    committed.project_assistant_message(
                        message_id.clone(),
                        text.trim(),
                        Some("text".into()),
                    );
                }
            }
            TranscriptEvent::Reasoning { text, message_id } => {
                if !text.trim().is_empty() {
                    committed.project_assistant_message(
                        message_id.clone(),
                        text.trim(),
                        Some("reasoning".into()),
                    );
                }
            }
            TranscriptEvent::ToolCall {
                text,
                tool_call_cards,
                persist_text_id,
                ..
            } => {
                if let Some(message_id) = persist_text_id
                    && !text.trim().is_empty()
                {
                    committed.project_assistant_message(
                        message_id.clone(),
                        text.trim(),
                        Some("text".into()),
                    );
                }
                for card in tool_call_cards {
                    committed.project_tool_call_step(
                        card.step_id.clone(),
                        ctx.step_num,
                        card.tool_index,
                        card.tool_name.clone(),
                        card.tool_input.to_string(),
                        card.tool_call_id.clone(),
                        card.is_high_risk,
                        card.silent,
                    );
                }
            }
            TranscriptEvent::ToolResult {
                history_observation,
                observation_card,
                ..
            } => {
                if let Some(card) = observation_card
                    && card.tool_name == "ask"
                    && !history_observation.trim().is_empty()
                {
                    committed.project_assistant_message(
                        card.step_id.clone(),
                        history_observation.trim(),
                        Some("text".into()),
                    );
                }
            }
            TranscriptEvent::UserInject {
                source, message_id, ..
            } => {
                if *source != InjectSource::ToolRunResult {
                    if let Some(message_id) = message_id {
                        committed.acknowledge_pending_user_input(message_id.clone());
                    }
                    committed.project_thought_step(
                        message_id
                            .clone()
                            .unwrap_or_else(|| haven_common::types::new_id("step")),
                        ctx.step_num,
                    );
                }
            }
            TranscriptEvent::CompactSummary { .. } => {}
        }
        Ok(committed)
    }

    /// Commit the durable row, publish its sequenced UI cards, then project canonical state.
    ///
    /// All durable assistant/thought/ask/reasoning chat rows for the ReAct
    /// loop are written here (X12). See module docs for the few documented
    /// exceptions (ingress seed, error partials, terminal ToolRun-result).
    pub(super) async fn apply_transcript(
        &self,
        ctx: &StepCtx,
        event: TranscriptEvent,
        state: &mut ReActState,
    ) -> anyhow::Result<()> {
        let (event, record, media_record, committed) =
            self.build_transcript_item(ctx, event).await?;
        // Persist the event and its materialized rows before mutating the
        // in-memory projection. A snapshot checkpoint may lag, but a
        // committed event is replayable after a crash and cannot be lost with
        // the RAM state.
        let write_result = {
            let _timer = self.metrics.start(
                MetricsPhase::EventAppend,
                &ctx.session_id,
                ctx.run_id,
                ctx.step_num,
            );
            self.event_store
                .commit_transcript_cancellable(
                    &ctx.session_id,
                    committed,
                    state.turn_cancel.clone(),
                )
                .await?
        };
        self.metrics.observe(
            MetricsPhase::SqliteLockWait,
            std::time::Duration::from_millis(write_result.lock_wait_ms),
        );
        self.committed_ui
            .publish(&ctx.emitter, &write_result.events)
            .await;
        let result = self
            .apply_transcript_projection(
                ctx,
                event,
                state,
                TranscriptProjection {
                    record,
                    persisted_media_record: media_record,
                },
            )
            .await;
        if result.is_err() {
            self.metrics.increment(MetricsCounter::ProjectionFailures);
        }
        result
    }

    /// Commit and publish one completed tool result without appending it to
    /// the model-facing canonical projection yet. Parallel results may arrive
    /// in any order; `ToolBatchState` applies the committed events to canonical
    /// in assistant call order once the batch has drained.
    pub(super) async fn commit_tool_result(
        &self,
        ctx: &StepCtx,
        event: TranscriptEvent,
        state: &mut ReActState,
    ) -> anyhow::Result<()> {
        anyhow::ensure!(
            matches!(&event, TranscriptEvent::ToolResult { .. }),
            "tool-result commit requires a ToolResult transcript event"
        );
        let (_, record, _, committed) = self.build_transcript_item(ctx, event).await?;
        let write_result = {
            let _timer = self.metrics.start(
                MetricsPhase::EventAppend,
                &ctx.session_id,
                ctx.run_id,
                ctx.step_num,
            );
            self.event_store
                .commit_transcript_cancellable(
                    &ctx.session_id,
                    committed,
                    state.turn_cancel.clone(),
                )
                .await?
        };
        self.metrics.observe(
            MetricsPhase::SqliteLockWait,
            std::time::Duration::from_millis(write_result.lock_wait_ms),
        );
        self.committed_ui
            .publish(&ctx.emitter, &write_result.events)
            .await;
        state.push_event(record);
        Ok(())
    }

    /// Batch context injections in one event/projection transaction. The
    /// resulting durable event order is the same as applying each item in
    /// order; media-plan records stay directly after their owning injection.
    pub(super) async fn apply_transcript_batch(
        &self,
        ctx: &StepCtx,
        events: Vec<TranscriptEvent>,
        state: &mut ReActState,
    ) -> anyhow::Result<()> {
        if events.is_empty() {
            return Ok(());
        }
        let mut committed = SessionCommitted::default();
        let mut projected = Vec::with_capacity(events.len());
        for event in events {
            let (event, record, media_record, item_commit) =
                self.build_transcript_item(ctx, event).await?;
            committed.events.extend(item_commit.events);
            committed.projections.extend(item_commit.projections);
            projected.push((event, record, media_record));
        }
        let write_result = {
            let _timer = self.metrics.start(
                MetricsPhase::EventAppend,
                &ctx.session_id,
                ctx.run_id,
                ctx.step_num,
            );
            self.event_store
                .commit_transcript_cancellable(
                    &ctx.session_id,
                    committed,
                    state.turn_cancel.clone(),
                )
                .await?
        };
        self.metrics.observe(
            MetricsPhase::SqliteLockWait,
            std::time::Duration::from_millis(write_result.lock_wait_ms),
        );
        self.committed_ui
            .publish(&ctx.emitter, &write_result.events)
            .await;
        for (event, record, media_record) in projected {
            let result = self
                .apply_transcript_projection(
                    ctx,
                    event,
                    state,
                    TranscriptProjection {
                        record,
                        persisted_media_record: media_record,
                    },
                )
                .await;
            if result.is_err() {
                self.metrics.increment(MetricsCounter::ProjectionFailures);
            }
            result?;
        }
        Ok(())
    }

    async fn apply_transcript_projection(
        &self,
        ctx: &StepCtx,
        event: TranscriptEvent,
        state: &mut ReActState,
        projection: TranscriptProjection,
    ) -> anyhow::Result<()> {
        let TranscriptProjection {
            record,
            persisted_media_record,
        } = projection;
        let _timer = self.metrics.start(
            MetricsPhase::Projection,
            &ctx.session_id,
            ctx.run_id,
            ctx.step_num,
        );
        match event {
            TranscriptEvent::Thought { .. } => {
                state.push_event(record);
            }
            TranscriptEvent::Reasoning { .. } => {
                state.push_event(record);
            }
            TranscriptEvent::ToolCall {
                text,
                tool_calls,
                reasoning,
                web_search_calls,
                thinking_blocks,
                tool_call_cards: _,
                persist_text_id: _,
            } => {
                state.push_event(record);
                Arc::make_mut(&mut state.canonical).push(CanonicalMessage::assistant(
                    vec![ContentPart::text(text)],
                    if tool_calls.is_empty() {
                        None
                    } else {
                        Some(tool_calls)
                    },
                    reasoning,
                    web_search_calls,
                    thinking_blocks,
                ));
                state.mark_canonical_append();
            }
            TranscriptEvent::ToolResult {
                canonical_observation,
                history_observation: _,
                tool_call_id,
                tool_call,
                tool_index: _,
                step_id: _,
                observation_card: _,
            } => {
                state.push_event(record);
                let is_final = tool_call.is_final || tool_call.tool_name == "final_answer";
                if !is_final {
                    Arc::make_mut(&mut state.canonical).push(CanonicalMessage::tool(
                        vec![ContentPart::text(canonical_observation)],
                        tool_call_id,
                    ));
                    state.mark_canonical_append();
                }
            }
            TranscriptEvent::UserInject {
                source,
                text,
                attachments,
                message_id: _,
            } => {
                // Supplement and any ingress MediaPlan were already published
                // from the committed rows. Canonical updates stay here so a
                // later projection failure cannot drop those live UI events.
                state.push_event(record);
                let strategy = self.media_strategy();
                let media_was_persisted = persisted_media_record.is_some();
                let media_record = match persisted_media_record {
                    Some(record) => Some(record),
                    None => media_record_for_inject(ctx.step_num, &attachments, strategy),
                };
                if let Some(media_record) = media_record {
                    if !media_was_persisted {
                        // The batch did not include this plan. Append it and
                        // publish that sequence here so a test without the
                        // store bridge still emits the card.
                        let sequence = self
                            .append_transcript_record(
                                &ctx.session_id,
                                &media_record,
                                ctx.run_id,
                                ctx.step_num,
                            )
                            .await?;
                        if sequence > 0 {
                            let committed = SessionEvent {
                                session_id: ctx.session_id.clone(),
                                sequence,
                                event_type: TRANSCRIPT_EVENT_TYPE.to_string(),
                                event_version: CURRENT_EVENT_VERSION,
                                payload: serde_json::to_string(&media_record)?,
                                created_at: String::new(),
                                run_id: Some(ctx.run_id),
                                step_number: Some(ctx.step_num),
                            };
                            self.committed_ui
                                .publish(&ctx.emitter, std::slice::from_ref(&committed))
                                .await;
                        }
                    }
                    state.push_event(media_record);
                }
                let mut content = vec![ContentPart::text(text)];
                for attachment in &attachments {
                    let input = haven_common::media::message_attachment_to_media_input(attachment);
                    crate::types::append_media_projection(&mut content, &input, strategy);
                }
                Arc::make_mut(&mut state.canonical)
                    .push(CanonicalMessage::user_with_source(content, source));
                state.mark_canonical_append();
            }
            TranscriptEvent::CompactSummary {
                compacted,
                media_inputs: _media_inputs,
                summary,
                tokens_before: _,
                tokens_after: _,
                episode_id,
                degraded: _,
            } => {
                // Replace the log with the CompactSummary root so pre-compaction
                // events (and embedded prior CompactSummaries) do not grow forever.
                // The sequenced Compaction event was published from the committed row.
                state.replace_with_compaction(record, compacted);
                self.persist_compaction_summary(&ctx.session_id, &summary, &episode_id)
                    .await;
            }
        }
        Ok(())
    }
}

/// Add a durably committed tool result to the next model request. The caller
/// supplies results in the original assistant call order, independent of
/// their event sequence / UI publication order.
pub(super) fn project_tool_result_canonical(state: &mut ReActState, event: &TranscriptEvent) {
    if let TranscriptEvent::ToolResult {
        canonical_observation,
        tool_call_id,
        tool_call,
        ..
    } = event
        && !tool_call.is_final
        && tool_call.tool_name != "final_answer"
    {
        Arc::make_mut(&mut state.canonical).push(CanonicalMessage::tool(
            vec![ContentPart::text(canonical_observation.clone())],
            tool_call_id.clone(),
        ));
        state.mark_canonical_append();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::AgentEventEmitter;
    use crate::types::{BranchPoint, project_transcript};
    use async_trait::async_trait;
    use haven_memory::Database;
    use std::sync::Arc;

    struct NoopEmitter;
    #[async_trait]
    impl AgentEventEmitter for NoopEmitter {
        async fn emit(&self, _event: crate::event::AgentEvent) {}
    }

    struct RecordingEmitter {
        events: Arc<std::sync::Mutex<Vec<crate::event::AgentEvent>>>,
    }
    #[async_trait]
    impl AgentEventEmitter for RecordingEmitter {
        async fn emit(&self, event: crate::event::AgentEvent) {
            self.events.lock().unwrap().push(event);
        }
    }

    fn step_ctx(session_id: &str) -> StepCtx {
        StepCtx {
            session_id: session_id.to_string(),
            step_num: 1,
            run_id: 1,
            emitter: Arc::new(NoopEmitter),
        }
    }

    fn test_engine(db: Arc<Database>) -> ReActEngine {
        let router = Arc::new(haven_llm::LlmRouter::new(
            haven_common::config::RouterConfig::default(),
        ));
        let executor = Arc::new(crate::session::SessionSupervisor::new_for_test(
            db.clone(),
            Arc::new(haven_tools::ToolsManager::new()),
            2,
        ));
        ReActEngine::new(
            router,
            crate::react::test_tool_catalog_port(&executor),
            executor,
            haven_memory::MemoryStore::new(db.clone()),
            10,
            haven_common::config::ContextLimitsConfig::default(),
        )
    }

    #[tokio::test]
    async fn event_boundary_store_ports_seed_append_and_replay_transcript_records() {
        let db = Arc::new(Database::open_in_memory().unwrap());
        let session = db.create_session("event boundary").unwrap();
        let engine = test_engine(db);
        let mut live = engine.event_store.subscribe();
        let seeded = TranscriptRecord::Thought {
            step_number: 1,
            text: "seed".into(),
            message_id: haven_common::types::new_id("step"),
        };
        let appended = TranscriptRecord::Reasoning {
            step_number: 2,
            text: "appended".into(),
            message_id: haven_common::types::new_id("msg"),
        };

        engine
            .seed_transcript_events(&session.id, std::slice::from_ref(&seeded), 7)
            .await
            .unwrap();
        assert_eq!(live.try_recv().unwrap().sequence, 1);
        assert_eq!(
            engine
                .append_transcript_record(&session.id, &appended, 8, 2)
                .await
                .unwrap(),
            2
        );
        let live_append = live.try_recv().unwrap();
        assert_eq!(live_append.sequence, 2);
        assert_eq!(live_append.run_id, Some(8));
        assert_eq!(live_append.step_number, Some(2));

        engine
            .event_store
            .append_branch_point(&session.id, 1, 1, Some("2026-09-24T00:00:00Z"), None)
            .unwrap();
        let replay = engine
            .load_durable_event_state(&session.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            serde_json::to_value(replay.events).unwrap(),
            serde_json::json!([seeded, appended])
        );
        assert_eq!(replay.cursor.event_sequence, 3);
        assert_eq!(replay.cursor.event_cursor, 2);
        let branch_point = replay.branch_points.get(&1).unwrap();
        assert_eq!(branch_point.event_cursor, 1);
        assert_eq!(branch_point.step_number, 1);
        assert_eq!(
            branch_point.last_msg_at.as_deref(),
            Some("2026-09-24T00:00:00Z")
        );
    }

    #[tokio::test]
    async fn transcript_append_errors_for_missing_session() {
        let db = Arc::new(Database::open_in_memory().unwrap());
        let engine = test_engine(db);
        let missing_session_id = haven_common::types::new_id("ses");
        let mut live = engine.event_store.subscribe();
        let record = TranscriptRecord::Thought {
            step_number: 1,
            text: "isolated test".into(),
            message_id: haven_common::types::new_id("step"),
        };

        assert!(
            engine
                .append_transcript_record(&missing_session_id, &record, 1, 1)
                .await
                .is_err()
        );
        assert!(
            engine
                .event_store
                .read_all(&missing_session_id)
                .unwrap()
                .is_empty()
        );
        assert!(matches!(
            live.try_recv(),
            Err(tokio::sync::broadcast::error::TryRecvError::Empty)
        ));
    }

    #[tokio::test]
    async fn transcript_append_validation_failure_has_no_durable_or_live_side_effect() {
        let db = Arc::new(Database::open_in_memory().unwrap());
        let session = db.create_session("event boundary failure").unwrap();
        let engine = test_engine(db.clone());
        let mut live = engine.event_store.subscribe();
        let oversized = TranscriptRecord::Thought {
            step_number: 1,
            text: "x".repeat(4 * 1024 * 1024 + 1),
            message_id: haven_common::types::new_id("step"),
        };

        let error = engine
            .append_transcript_record(&session.id, &oversized, 1, 1)
            .await
            .unwrap_err();

        assert!(error.to_string().contains("transcript batch exceeds"));
        assert!(engine.event_store.read_all(&session.id).unwrap().is_empty());
        assert!(db.get_session_messages(&session.id).unwrap().is_empty());
        assert!(matches!(
            live.try_recv(),
            Err(tokio::sync::broadcast::error::TryRecvError::Empty)
        ));
    }

    #[tokio::test]
    async fn durable_event_replay_keeps_agent_transcript_validation_and_error_text() {
        let db = Arc::new(Database::open_in_memory().unwrap());
        let session = db.create_session("invalid event boundary replay").unwrap();
        let engine = test_engine(db);
        engine
            .event_store
            .append(
                &session.id,
                haven_memory::TRANSCRIPT_EVENT_TYPE,
                r#"{"not_a_transcript_record":true}"#,
                Some(1),
                Some(1),
            )
            .unwrap();

        let error = engine
            .load_durable_event_state(&session.id)
            .await
            .unwrap_err();

        assert!(error.to_string().starts_with(&format!(
            "invalid transcript event 1 for session {}:",
            session.id
        )));
    }

    #[tokio::test]
    async fn compact_summary_event_requires_payload_step_number() {
        let db = Arc::new(Database::open_in_memory().unwrap());
        let session = db.create_session("compact summary missing step").unwrap();
        let engine = test_engine(db);
        engine
            .event_store
            .append(
                &session.id,
                haven_memory::TRANSCRIPT_EVENT_TYPE,
                r#"{"type":"compact_summary","compacted":[],"summary":"summary","tokens_before":100,"tokens_after":20,"episode_id":"msg-summary","degraded":false}"#,
                Some(3),
                Some(17),
            )
            .unwrap();

        let error = engine
            .load_durable_event_state(&session.id)
            .await
            .unwrap_err();

        assert!(error.to_string().contains("missing field `step_number`"));
    }

    #[tokio::test]
    async fn apply_user_inject_sets_source_raw_text() {
        let dir = std::env::temp_dir().join(format!(
            "haven_transcript_inject_{}.db",
            uuid::Uuid::new_v4()
        ));
        let db = Arc::new(Database::open(&dir).unwrap());
        let session = db.create_session("t").unwrap();
        let engine = test_engine(db);
        let ctx = step_ctx(&session.id);
        let mut state = ReActState::new(Vec::new(), Vec::new(), std::collections::HashMap::new());
        engine
            .apply_transcript(
                &ctx,
                TranscriptEvent::UserInject {
                    source: InjectSource::Steering,
                    text: "be brief".into(),
                    attachments: vec![],
                    message_id: None,
                },
                &mut state,
            )
            .await
            .unwrap();
        assert_eq!(state.canonical.len(), 1);
        assert_eq!(state.canonical[0].source, Some(InjectSource::Steering));
        let text = match &state.canonical[0].content[0] {
            ContentPart::Text(t) => t.as_str(),
            _ => panic!("expected text"),
        };
        assert_eq!(text, "be brief");
        assert_eq!(state.events.len(), 1);
    }

    #[tokio::test]
    async fn apply_tool_run_result_keeps_self_labelled_body() {
        let dir = std::env::temp_dir().join(format!(
            "haven_transcript_tool_run_{}.db",
            uuid::Uuid::new_v4()
        ));
        let db = Arc::new(Database::open(&dir).unwrap());
        let session = db.create_session("t").unwrap();
        let engine = test_engine(db);
        let ctx = step_ctx(&session.id);
        let mut state = ReActState::new(Vec::new(), Vec::new(), std::collections::HashMap::new());
        let body = "[Background tool run result]\ntool_run_id=toolrun-1\nok";
        engine
            .apply_transcript(
                &ctx,
                TranscriptEvent::UserInject {
                    source: InjectSource::ToolRunResult,
                    text: body.into(),
                    attachments: vec![],
                    message_id: Some(crate::react::tool_run_result_message_id("toolrun-1")),
                },
                &mut state,
            )
            .await
            .unwrap();
        assert_eq!(state.canonical[0].source, Some(InjectSource::ToolRunResult));
        let text = match &state.canonical[0].content[0] {
            ContentPart::Text(t) => t.as_str(),
            _ => panic!("expected text"),
        };
        assert_eq!(text, body);
        assert!(!text.starts_with("Background tool run result: "));
    }

    #[tokio::test]
    async fn apply_tool_run_result_emits_supplement_without_thought_step() {
        let dir = std::env::temp_dir().join(format!(
            "haven_transcript_tool_run_supp_{}.db",
            uuid::Uuid::new_v4()
        ));
        let db = Arc::new(Database::open(&dir).unwrap());
        let session = db.create_session("t").unwrap();
        let recorded = Arc::new(std::sync::Mutex::new(Vec::new()));
        let engine = test_engine(db.clone());
        let mut ctx = step_ctx(&session.id);
        ctx.emitter = Arc::new(RecordingEmitter {
            events: recorded.clone(),
        });
        let mut state = ReActState::new(Vec::new(), Vec::new(), std::collections::HashMap::new());
        let body = "[Background tool run result]\ntool_run_id: toolrun-9\nstatus: completed\n\nok";
        engine
            .apply_transcript(
                &ctx,
                TranscriptEvent::UserInject {
                    source: InjectSource::ToolRunResult,
                    text: body.into(),
                    attachments: vec![],
                    message_id: Some(crate::react::tool_run_result_message_id("toolrun-9")),
                },
                &mut state,
            )
            .await
            .unwrap();
        let emitted = recorded.lock().unwrap().clone();
        assert!(
            emitted.iter().any(|e| matches!(
                e,
                crate::event::AgentEvent::Supplement {
                    inject_source: Some(InjectSource::ToolRunResult),
                    ..
                }
            )),
            "ToolRunResult must emit Supplement for in-chat wake visibility"
        );
        let steps = db.get_session_steps(&session.id).unwrap_or_default();
        assert!(
            steps.is_empty(),
            "ToolRunResult must not create a thought step"
        );
    }

    #[tokio::test]
    async fn apply_thought_appends_event_not_canonical() {
        let dir = std::env::temp_dir().join(format!(
            "haven_transcript_thought_{}.db",
            uuid::Uuid::new_v4()
        ));
        let db = Arc::new(Database::open(&dir).unwrap());
        let session = db.create_session("t").unwrap();
        let mid = haven_common::types::new_id("step");
        let engine = test_engine(db.clone());
        let ctx = step_ctx(&session.id);
        let mut state = ReActState::new(Vec::new(), Vec::new(), std::collections::HashMap::new());
        engine
            .apply_transcript(
                &ctx,
                TranscriptEvent::Thought {
                    text: "thinking".into(),
                    message_id: mid.clone(),
                },
                &mut state,
            )
            .await
            .unwrap();
        assert_eq!(state.events.len(), 1);
        let (_, rounds) = project_transcript(&state.events);
        assert_eq!(rounds.len(), 1);
        assert_eq!(rounds[0].thought.as_deref(), Some("thinking"));
        assert!(state.canonical.is_empty());
        // X12: Thought projects the messages row under the shared id.
        let msgs = db.get_session_messages(&session.id).unwrap();
        assert!(
            msgs.iter()
                .any(|m| m.id == mid && m.content == "thinking" && m.role == "assistant"),
            "expected projected thought message, got {msgs:?}"
        );
        assert!(
            db.get_session_steps(&session.id)
                .unwrap()
                .iter()
                .any(|step| step.id == mid && step.tool_name.is_none()),
            "the thought message and execution step must share the committed id"
        );
    }

    #[tokio::test]
    async fn apply_reasoning_projects_message_not_canonical() {
        let dir = std::env::temp_dir().join(format!(
            "haven_transcript_reasoning_{}.db",
            uuid::Uuid::new_v4()
        ));
        let db = Arc::new(Database::open(&dir).unwrap());
        let session = db.create_session("t").unwrap();
        let mid = haven_common::types::new_id("msg");
        let engine = test_engine(db.clone());
        let ctx = step_ctx(&session.id);
        let mut state = ReActState::new(Vec::new(), Vec::new(), std::collections::HashMap::new());
        engine
            .apply_transcript(
                &ctx,
                TranscriptEvent::Reasoning {
                    text: "why".into(),
                    message_id: mid.clone(),
                },
                &mut state,
            )
            .await
            .unwrap();
        assert_eq!(state.events.len(), 1);
        assert!(matches!(
            &state.events[0],
            TranscriptRecord::Reasoning { message_id, .. } if message_id == &mid
        ));
        assert!(state.canonical.is_empty());
        let (canon, rounds) = project_transcript(&state.events);
        assert!(canon.is_empty());
        assert!(rounds.is_empty());
        let msgs = db.get_session_messages(&session.id).unwrap();
        assert!(
            msgs.iter().any(|m| {
                m.id == mid && m.content == "why" && m.message_type.as_deref() == Some("reasoning")
            }),
            "expected projected reasoning message, got {msgs:?}"
        );
    }

    #[tokio::test]
    async fn apply_compact_summary_replaces_canonical() {
        let dir = std::env::temp_dir().join(format!(
            "haven_transcript_compact_{}.db",
            uuid::Uuid::new_v4()
        ));
        let db = Arc::new(Database::open(&dir).unwrap());
        let session = db.create_session("t").unwrap();
        let ui_events = Arc::new(std::sync::Mutex::new(Vec::new()));
        let engine = test_engine(db);
        let ctx = StepCtx {
            session_id: session.id.clone(),
            step_num: 2,
            run_id: 1,
            emitter: Arc::new(RecordingEmitter {
                events: ui_events.clone(),
            }),
        };
        let canonical = vec![
            CanonicalMessage::user_text("old1"),
            CanonicalMessage::assistant(
                vec![ContentPart::text("old2")],
                None,
                None,
                Vec::new(),
                Vec::new(),
            ),
        ];
        let events = vec![TranscriptRecord::Thought {
            step_number: 1,
            text: "keep".into(),
            message_id: "step-keep".into(),
        }];
        let compacted = vec![
            CanonicalMessage::assistant(
                vec![ContentPart::text("SUMMARY: prior turns")],
                None,
                None,
                Vec::new(),
                Vec::new(),
            ),
            CanonicalMessage::user_text("recent"),
        ];
        let mut state = ReActState::new(events, canonical, std::collections::HashMap::new());
        state.branch_points.insert(
            1,
            BranchPoint {
                event_cursor: 1,
                step_number: 1,
                last_msg_at: None,
            },
        );
        engine
            .apply_transcript(
                &ctx,
                TranscriptEvent::CompactSummary {
                    compacted: compacted.clone(),
                    media_inputs: Vec::new(),
                    summary: "prior turns".into(),
                    tokens_before: 100,
                    tokens_after: 40,
                    episode_id: haven_common::types::new_id("msg"),
                    degraded: false,
                },
                &mut state,
            )
            .await
            .unwrap();
        assert_eq!(state.canonical.len(), 2);
        // CompactSummary replaces the event log (no pre-compaction growth).
        assert_eq!(state.events.len(), 1);
        assert!(matches!(
            &state.events[0],
            TranscriptRecord::CompactSummary { .. }
        ));
        assert!(state.branch_points.is_empty());
        let (_, rounds) = project_transcript(&state.events);
        assert!(rounds.is_empty());
        let durable_events = engine
            .event_store
            .read_active_transcript(&session.id)
            .unwrap();
        let durable_summary = durable_events
            .first()
            .expect("committed compact summary event");
        assert_eq!(durable_summary.step_number, Some(2));
        assert!(matches!(
            serde_json::from_str::<TranscriptRecord>(&durable_summary.payload).unwrap(),
            TranscriptRecord::CompactSummary { step_number: 2, .. }
        ));
        let durable_sequence = durable_summary.sequence as u64;
        let ev = ui_events.lock().unwrap();
        assert!(
            ev.iter().any(|e| matches!(
                e,
                crate::event::AgentEvent::Compaction {
                    tokens_before: 100,
                    tokens_after: 40,
                    event_seq: Some(sequence),
                    ..
                } if *sequence == durable_sequence
            )),
            "expected Compaction event, got {ev:?}"
        );
    }

    #[tokio::test]
    async fn apply_tool_call_defers_tool_call_cards_until_requested() {
        let dir = std::env::temp_dir().join(format!(
            "haven_transcript_toolcall_{}.db",
            uuid::Uuid::new_v4()
        ));
        let db = Arc::new(Database::open(&dir).unwrap());
        let session = db.create_session("t").unwrap();
        let ui_events = Arc::new(std::sync::Mutex::new(Vec::new()));
        let engine = test_engine(db);
        let ctx = StepCtx {
            session_id: session.id.clone(),
            step_num: 3,
            run_id: 7,
            emitter: Arc::new(RecordingEmitter {
                events: ui_events.clone(),
            }),
        };
        let step_id = haven_common::types::new_id("step");
        let mut state = ReActState::new(Vec::new(), Vec::new(), std::collections::HashMap::new());
        engine
            .apply_transcript(
                &ctx,
                TranscriptEvent::ToolCall {
                    text: "calling".into(),
                    tool_calls: vec![CanonicalToolCall {
                        id: "call-1".into(),
                        name: "echo".into(),
                        arguments: serde_json::json!({"x": 1}),
                    }],
                    reasoning: None,
                    web_search_calls: vec![],
                    thinking_blocks: vec![],
                    tool_call_cards: vec![ToolCallCard {
                        tool_name: "echo".into(),
                        tool_input: serde_json::json!({"x": 1}),
                        tool_call_id: Some("call-1".into()),
                        step_id: step_id.clone(),
                        tool_index: 0,
                        suppress_streamed_thought: false,
                        is_high_risk: false,
                        silent: false,
                    }],
                    persist_text_id: None,
                },
                &mut state,
            )
            .await
            .unwrap();
        assert_eq!(state.canonical.len(), 1);
        assert_eq!(state.events.len(), 1);
        let durable_sequence = engine
            .event_store
            .read_active_transcript(&session.id)
            .unwrap()
            .first()
            .expect("committed tool-call event")
            .sequence as u64;
        assert!(
            ui_events.lock().unwrap().is_empty(),
            "ToolCall cards should wait until each tool is about to start"
        );
        engine
            .committed_ui
            .publish_tool_call(&ctx.emitter, &session.id, &step_id)
            .await;
        let ev = ui_events.lock().unwrap();
        assert!(
            ev.iter().any(|e| matches!(
                e,
                crate::event::AgentEvent::ToolCall {
                    tool_name,
                    step_id: sid,
                    event_seq: Some(sequence),
                    ..
                } if tool_name == "echo" && sid == &step_id && *sequence == durable_sequence
            )),
            "expected ToolCall card, got {ev:?}"
        );
    }

    #[tokio::test]
    async fn apply_tool_result_emits_observation_then_projects() {
        let dir = std::env::temp_dir().join(format!(
            "haven_transcript_toolresult_{}.db",
            uuid::Uuid::new_v4()
        ));
        let db = Arc::new(Database::open(&dir).unwrap());
        let session = db.create_session("t").unwrap();
        let ui_events = Arc::new(std::sync::Mutex::new(Vec::new()));
        let engine = test_engine(db);
        let ctx = StepCtx {
            session_id: session.id.clone(),
            step_num: 4,
            run_id: 2,
            emitter: Arc::new(RecordingEmitter {
                events: ui_events.clone(),
            }),
        };
        let step_id = haven_common::types::new_id("step");
        let mut state = ReActState::new(Vec::new(), Vec::new(), std::collections::HashMap::new());
        let tool_call = ToolCall {
            tool_name: "echo".into(),
            tool_input: serde_json::json!({}),
            is_final: false,
            tool_call_id: Some("call-2".into()),
        };
        engine
            .apply_transcript(
                &ctx,
                TranscriptEvent::ToolResult {
                    canonical_observation: r#"{"ok":true}"#.into(),
                    history_observation: "ok".into(),
                    tool_call_id: Some("call-2".into()),
                    tool_call,
                    tool_index: 0,
                    step_id: step_id.clone(),
                    observation_card: Some(Box::new(ObservationCard {
                        tool_name: "echo".into(),
                        tool_call_id: Some("call-2".into()),
                        step_id: step_id.clone(),
                        tool_index: 0,
                        silent: false,
                        ask_options: vec![],
                        outcome: haven_tools::ToolExecutionOutcome::Succeeded,
                        idempotency: haven_tools::OperationIdempotency::Idempotent,
                        operation_scope: haven_tools::ToolOperationScope::Session,
                        renderer: "generic".into(),
                        result_envelope: haven_tools::ToolResultEnvelope::from_parts(
                            haven_tools::ToolExecutionOutcome::Succeeded,
                            None,
                            haven_tools::ToolRetryability::Unknown,
                            haven_tools::OperationIdempotency::Idempotent,
                        ),
                    })),
                },
                &mut state,
            )
            .await
            .unwrap();
        assert_eq!(state.canonical.len(), 1);
        let (_, rounds) = project_transcript(&state.events);
        assert_eq!(rounds.len(), 1);
        assert_eq!(rounds[0].tools[0].observation.as_deref(), Some("ok"));
        assert_eq!(rounds[0].tools[0].tool_index, 0);
        assert_eq!(rounds[0].tools[0].step_id, step_id);
        let durable_sequence = engine
            .event_store
            .read_active_transcript(&session.id)
            .unwrap()
            .first()
            .expect("committed tool-result event")
            .sequence as u64;
        let ev = ui_events.lock().unwrap();
        assert!(
            ev.iter().any(|e| matches!(
                e,
                crate::event::AgentEvent::Observation {
                    observation,
                    step_id: sid,
                    event_seq: Some(sequence),
                    ..
                } if observation == "ok" && sid == &step_id && *sequence == durable_sequence
            )),
            "expected Observation card, got {ev:?}"
        );
    }

    #[tokio::test]
    async fn apply_parallel_tool_results_one_round() {
        let dir = std::env::temp_dir().join(format!(
            "haven_transcript_parallel_{}.db",
            uuid::Uuid::new_v4()
        ));
        let db = Arc::new(Database::open(&dir).unwrap());
        let session = db.create_session("t").unwrap();
        let engine = test_engine(db);
        let ctx = step_ctx(&session.id);
        let mut state = ReActState::new(Vec::new(), Vec::new(), std::collections::HashMap::new());
        engine
            .apply_transcript(
                &ctx,
                TranscriptEvent::Thought {
                    text: "both".into(),
                    message_id: haven_common::types::new_id("step"),
                },
                &mut state,
            )
            .await
            .unwrap();
        let events = [("c1", "a"), ("c2", "b")]
            .into_iter()
            .map(|(id, name)| TranscriptEvent::ToolResult {
                canonical_observation: format!("r{name}"),
                history_observation: format!("r{name}"),
                tool_call_id: Some(id.into()),
                tool_call: ToolCall {
                    tool_name: name.into(),
                    tool_input: serde_json::json!({}),
                    is_final: false,
                    tool_call_id: Some(id.into()),
                },
                tool_index: if id == "c1" { 0 } else { 1 },
                step_id: format!("step-{id}"),
                observation_card: None,
            })
            .collect();
        engine
            .apply_transcript_batch(&ctx, events, &mut state)
            .await
            .unwrap();
        assert_eq!(
            engine
                .event_store
                .read_active_transcript(&session.id)
                .unwrap()
                .len(),
            3,
            "thought plus two tool results must share one ordered batch"
        );
        let (_, rounds) = project_transcript(&state.events);
        assert_eq!(rounds.len(), 1);
        assert_eq!(rounds[0].tools.len(), 2);
        assert_eq!(state.canonical.len(), 2);
    }

    #[tokio::test]
    async fn apply_ask_tool_result_projects_question_under_step_id() {
        let dir = std::env::temp_dir().join(format!(
            "haven_transcript_ask_proj_{}.db",
            uuid::Uuid::new_v4()
        ));
        let db = Arc::new(Database::open(&dir).unwrap());
        let session = db.create_session("t").unwrap();
        let step_id = haven_common::types::new_id("step");
        let engine = test_engine(db.clone());
        let ctx = step_ctx(&session.id);
        let mut state = ReActState::new(Vec::new(), Vec::new(), std::collections::HashMap::new());
        engine
            .apply_transcript(
                &ctx,
                TranscriptEvent::ToolResult {
                    canonical_observation: r#"{"question":"Pick one?"}"#.into(),
                    history_observation: "Pick one?".into(),
                    tool_call_id: Some("call-ask".into()),
                    tool_call: ToolCall {
                        tool_name: "ask".into(),
                        tool_input: serde_json::json!({"question":"Pick one?"}),
                        is_final: false,
                        tool_call_id: Some("call-ask".into()),
                    },
                    tool_index: 0,
                    step_id: step_id.clone(),
                    observation_card: Some(Box::new(ObservationCard {
                        tool_name: "ask".into(),
                        tool_call_id: Some("call-ask".into()),
                        step_id: step_id.clone(),
                        tool_index: 0,
                        silent: false,
                        ask_options: vec!["A".into(), "B".into()],
                        outcome: haven_tools::ToolExecutionOutcome::Succeeded,
                        idempotency: haven_tools::OperationIdempotency::Idempotent,
                        operation_scope: haven_tools::ToolOperationScope::Session,
                        renderer: "generic".into(),
                        result_envelope: haven_tools::ToolResultEnvelope::from_parts(
                            haven_tools::ToolExecutionOutcome::Succeeded,
                            None,
                            haven_tools::ToolRetryability::Unknown,
                            haven_tools::OperationIdempotency::Idempotent,
                        ),
                    })),
                },
                &mut state,
            )
            .await
            .unwrap();
        let msgs = db.get_session_messages(&session.id).unwrap();
        assert!(
            msgs.iter()
                .any(|m| m.id == step_id && m.content == "Pick one?" && m.role == "assistant"),
            "ask question must project under shared step id, got {msgs:?}"
        );
    }

    #[tokio::test]
    async fn thought_commit_rolls_back_event_and_message_when_step_projection_fails() {
        let dir = std::env::temp_dir().join(format!(
            "haven_transcript_thought_fail_{}.db",
            uuid::Uuid::new_v4()
        ));
        let db = Arc::new(Database::open(&dir).unwrap());
        let session = db.create_session("t").unwrap();
        let message_id = haven_common::types::new_id("step");
        db.create_thought_step(&session.id, 1, &message_id).unwrap();
        let recorded = Arc::new(std::sync::Mutex::new(Vec::new()));
        let engine = test_engine(db.clone());
        let mut ctx = step_ctx(&session.id);
        ctx.emitter = Arc::new(RecordingEmitter {
            events: recorded.clone(),
        });
        let dispatcher = Arc::new(EventDispatcher::new());
        dispatcher.set_emitter(ctx.emitter.clone());
        let mut committed_events = engine.event_store.subscribe();
        let ui_events = engine.event_store.subscribe();
        engine.start_committed_ui_bridge(dispatcher, ui_events);
        let mut state = ReActState::new(Vec::new(), Vec::new(), std::collections::HashMap::new());
        let error = engine
            .apply_transcript(
                &ctx,
                TranscriptEvent::Thought {
                    text: "keep".into(),
                    message_id: message_id.clone(),
                },
                &mut state,
            )
            .await;
        assert!(error.is_err(), "step collision must fail the transaction");
        let durable = engine
            .event_store
            .read_active_transcript(&session.id)
            .unwrap();
        assert!(durable.is_empty(), "the event must roll back with its step");
        assert!(db.get_session_messages(&session.id).unwrap().is_empty());
        assert_eq!(db.get_session_steps(&session.id).unwrap().len(), 1);
        assert!(recorded.lock().unwrap().is_empty());
        assert!(matches!(
            committed_events.try_recv(),
            Err(tokio::sync::broadcast::error::TryRecvError::Empty)
        ));
        assert!(state.events.is_empty());
    }

    #[tokio::test]
    async fn bridge_and_apply_publish_one_action() {
        let dir = std::env::temp_dir().join(format!(
            "haven_transcript_bridge_tool_run_{}.db",
            uuid::Uuid::new_v4()
        ));
        let db = Arc::new(Database::open(&dir).unwrap());
        let session = db.create_session("t").unwrap();
        let recorded = Arc::new(std::sync::Mutex::new(Vec::new()));
        let emitter: Arc<dyn AgentEventEmitter> = Arc::new(RecordingEmitter {
            events: recorded.clone(),
        });
        let engine = test_engine(db);
        let dispatcher = Arc::new(EventDispatcher::new());
        dispatcher.set_emitter(emitter.clone());
        let rx = engine.event_store.subscribe();
        engine.start_committed_ui_bridge(dispatcher, rx);
        let ctx = StepCtx {
            session_id: session.id.clone(),
            step_num: 2,
            run_id: 3,
            emitter,
        };
        let step_id = haven_common::types::new_id("step");
        let mut state = ReActState::new(Vec::new(), Vec::new(), std::collections::HashMap::new());
        engine
            .apply_transcript(
                &ctx,
                TranscriptEvent::ToolCall {
                    text: String::new(),
                    tool_calls: vec![CanonicalToolCall {
                        id: "call-1".into(),
                        name: "echo".into(),
                        arguments: serde_json::json!({}),
                    }],
                    reasoning: None,
                    web_search_calls: vec![],
                    thinking_blocks: vec![],
                    tool_call_cards: vec![ToolCallCard {
                        tool_name: "echo".into(),
                        tool_input: serde_json::json!({}),
                        tool_call_id: Some("call-1".into()),
                        step_id: step_id.clone(),
                        tool_index: 0,
                        suppress_streamed_thought: false,
                        is_high_risk: false,
                        silent: false,
                    }],
                    persist_text_id: None,
                },
                &mut state,
            )
            .await
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        engine
            .committed_ui
            .publish_tool_call(&ctx.emitter, &session.id, &step_id)
            .await;
        let events = recorded.lock().unwrap().clone();
        let tool_calls: Vec<_> = events
            .iter()
            .filter(|event| matches!(event, crate::event::AgentEvent::ToolCall { .. }))
            .collect();
        assert_eq!(
            tool_calls.len(),
            1,
            "the store bridge and apply_transcript must publish one ToolCall, got {events:?}"
        );
        let sequence = engine
            .event_store
            .read_active_transcript(&session.id)
            .unwrap()[0]
            .sequence as u64;
        assert!(
            matches!(
                tool_calls[0],
                crate::event::AgentEvent::ToolCall {
                    step_id: id,
                    event_seq: Some(event_seq),
                    ..
                } if id == &step_id && *event_seq == sequence
            ),
            "expected the committed ToolCall sequence, got {tool_calls:?}"
        );
    }
}
