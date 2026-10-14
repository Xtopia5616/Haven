//! Derive an independent session from a selected user message.
//!
//! Unlike rollback, this leaves the source session and its active event path
//! untouched. The child receives a re-keyed transcript prefix ending at the
//! selected input, so its pending run executes that input again.

use crate::AgentLayer;
use crate::react::ReActEngine;
use crate::types::TranscriptRecord;
use haven_common::lifecycle::SessionStatus;
use haven_common::types::{is_canonical_id, new_id};
use haven_memory::{SessionEventInput, SessionForkBranchPoint, SessionForkSnapshot};
use std::collections::{HashMap, HashSet};

impl AgentLayer {
    /// Create a new session whose active transcript ends with the selected
    /// user message. The source session is never rewound or modified.
    pub async fn fork_session_from_message(
        &self,
        session_id: &str,
        message_id: &str,
    ) -> anyhow::Result<String> {
        anyhow::ensure!(
            is_canonical_id(session_id, "ses"),
            "session_id must be a canonical ses-* ID"
        );
        anyhow::ensure!(
            is_canonical_id(message_id, "msg"),
            "message_id must be a canonical msg-* ID"
        );

        {
            let _lifecycle = self.executor.lifecycle_guard().await;
            self.executor.ensure_lifecycle_open()?;
            if self.executor.is_session_closing(session_id) {
                anyhow::bail!("session is closing; retry after deletion");
            }
        }

        let store = self.executor.session_store();
        let source_message = store
            .load_rollback_target_message(session_id, message_id)
            .await?;
        anyhow::ensure!(
            source_message.role == haven_common::types::CanonicalRole::User,
            "a new session can only be derived from a user message"
        );

        let mut durable = self
            .react_engine
            .load_durable_event_state(session_id)
            .await?
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "this session has no durable transcript and cannot be branched from"
                )
            })?;

        // A compaction root no longer retains the original event path. Only
        // messages after that root can be used as an exact replay boundary.
        let compacted_boundary = durable
            .events
            .iter()
            .rposition(|event| matches!(event, TranscriptRecord::CompactSummary { .. }));
        let search_start = compacted_boundary.map_or(0, |index| index + 1);
        let (target_index, target_step_number) = durable
            .events
            .iter()
            .enumerate()
            .skip(search_start)
            .find_map(|(index, event)| match event {
                TranscriptRecord::UserInject {
                    step_number,
                    message_id: Some(event_message_id),
                    ..
                } if event_message_id == message_id => Some((index, *step_number)),
                _ => None,
            })
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "the selected message is outside the restorable transcript path; it may have been compacted"
                )
            })?;
        durable.events.truncate(target_index + 1);
        let transcript_cursor = durable.events.len();
        let mut branch_points = durable
            .branch_points
            .into_values()
            .filter(|point| {
                point.event_cursor <= transcript_cursor && point.step_number <= target_step_number
            })
            .map(|point| SessionForkBranchPoint {
                event_cursor: point.event_cursor,
                step_number: point.step_number,
                source_last_msg_at: point.last_msg_at,
            })
            .collect::<Vec<_>>();
        branch_points.sort_by_key(|point| (point.event_cursor, point.step_number));

        let mut represented_ids = HashSet::new();
        let mut represented_steps = HashSet::new();
        for event in &durable.events {
            collect_record_identities(event, &mut represented_ids, &mut represented_steps);
        }
        anyhow::ensure!(
            represented_ids.contains(message_id),
            "selected user message is not represented in the active transcript"
        );

        let projection = store.session_resume_projection(session_id).await?;
        let mut messages = projection
            .messages
            .into_iter()
            .filter(|message| {
                represented_ids.contains(&message.message_id)
                    && (message.created_at <= source_message.created_at
                        || message.message_id == source_message.message_id)
            })
            .collect::<Vec<_>>();
        anyhow::ensure!(
            messages.iter().any(|message| {
                message.message_id == source_message.message_id
                    && message.role == haven_common::types::CanonicalRole::User
            }),
            "selected user message has no durable transcript projection"
        );
        let mut steps = projection
            .steps
            .into_iter()
            .filter(|step| {
                represented_steps.contains(&step.step_number)
                    && (step.created_at <= source_message.created_at
                        || step.step_id == source_message.message_id)
            })
            .collect::<Vec<_>>();

        let mut identities = HashMap::new();
        for event in &mut durable.events {
            remap_record_identities(event, &mut identities)?;
            event.validate_identity()?;
        }
        for message in &mut messages {
            message.message_id = remap_identity(&message.message_id, &mut identities)?;
        }
        for step in &mut steps {
            step.step_id = remap_identity(&step.step_id, &mut identities)?;
        }
        let child_source_message_id = identities
            .get(message_id)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("selected message identity was not re-keyed"))?;

        let transcript_events = durable
            .events
            .iter()
            .map(|event| {
                let mut input = ReActEngine::transcript_event_input(event, 0)?;
                // Imported history is not part of a runtime run instance.
                input.run_id = None;
                Ok(input)
            })
            .collect::<anyhow::Result<Vec<SessionEventInput>>>()?;
        let snapshot = SessionForkSnapshot {
            parent_session_id: session_id.to_owned(),
            source_message_id: message_id.to_owned(),
            source_message_created_at: source_message.created_at,
            child_source_message_id,
            expected_event_sequence: durable.cursor.event_sequence,
            expected_projection_created_at: durable.cursor.last_msg_at,
            input_text: source_message.content,
            transcript_events,
            branch_points,
            messages,
            steps,
        };
        let runtime_session = {
            // Serialize only the final commit/actor registration with session
            // lifecycle operations. The parent may keep running while the
            // snapshot is prepared; the store's event/projection watermarks
            // reject a stale snapshot without interrupting that run.
            let _lifecycle = self.executor.lifecycle_guard().await;
            self.executor.ensure_lifecycle_open()?;
            if self.executor.is_session_closing(session_id) {
                anyhow::bail!("session is closing; retry after deletion");
            }
            let child = store.create_fork_session(snapshot).await?;
            if let Err(error) = self
                .executor
                .ensure_session_loaded_locked(&child.session_id)
                .await
            {
                let _ = store.delete_session(&child.session_id).await;
                return Err(error);
            }
            self.spawn_title_generation(&child.session_id);
            self.executor
                .update_session_status(&child.session_id, SessionStatus::Pending)
                .await?;
            self.executor
                .get_runtime_session(&child.session_id)
                .await
                .ok_or_else(|| anyhow::anyhow!("derived session was not registered"))?
        };
        self.events.emit_session_created(&runtime_session).await;
        Ok(runtime_session.session_id)
    }
}

fn collect_record_identities(
    event: &TranscriptRecord,
    identities: &mut HashSet<String>,
    step_numbers: &mut HashSet<u32>,
) {
    match event {
        TranscriptRecord::Thought {
            step_number,
            message_id,
            ..
        } => {
            identities.insert(message_id.clone());
            step_numbers.insert(*step_number);
        }
        TranscriptRecord::Reasoning {
            step_number,
            message_id,
            ..
        } => {
            identities.insert(message_id.clone());
            step_numbers.insert(*step_number);
        }
        TranscriptRecord::ToolCall {
            step_number,
            message_id,
            ..
        } => {
            if let Some(message_id) = message_id {
                identities.insert(message_id.clone());
            }
            step_numbers.insert(*step_number);
        }
        TranscriptRecord::ToolResult {
            step_number,
            step_id,
            ..
        } => {
            identities.insert(step_id.clone());
            step_numbers.insert(*step_number);
        }
        TranscriptRecord::UserInject {
            step_number,
            message_id,
            ..
        } => {
            if let Some(message_id) = message_id {
                identities.insert(message_id.clone());
            }
            step_numbers.insert(*step_number);
        }
        TranscriptRecord::MediaPlan { step_number, .. } => {
            step_numbers.insert(*step_number);
        }
        TranscriptRecord::CompactSummary {
            step_number,
            compacted,
            ..
        } => {
            for message in compacted {
                if let Some(message_id) = &message.message_id {
                    identities.insert(message_id.clone());
                }
            }
            step_numbers.insert(*step_number);
        }
    }
}

fn remap_identity(
    source_id: &str,
    identities: &mut HashMap<String, String>,
) -> anyhow::Result<String> {
    if let Some(child_id) = identities.get(source_id) {
        return Ok(child_id.clone());
    }
    let prefix = if is_canonical_id(source_id, "step") {
        "step"
    } else if is_canonical_id(source_id, "msg") {
        "msg"
    } else {
        anyhow::bail!("cannot derive session with invalid transcript identity")
    };
    let child_id = new_id(prefix);
    identities.insert(source_id.to_owned(), child_id.clone());
    Ok(child_id)
}

fn remap_record_identities(
    event: &mut TranscriptRecord,
    identities: &mut HashMap<String, String>,
) -> anyhow::Result<()> {
    match event {
        TranscriptRecord::Thought { message_id, .. } => {
            *message_id = remap_identity(message_id, identities)?;
        }
        TranscriptRecord::Reasoning { message_id, .. } => {
            *message_id = remap_identity(message_id, identities)?;
        }
        TranscriptRecord::ToolCall { message_id, .. } => {
            if let Some(message_id) = message_id {
                *message_id = remap_identity(message_id, identities)?;
            }
        }
        TranscriptRecord::ToolResult { step_id, .. } => {
            *step_id = remap_identity(step_id, identities)?;
        }
        TranscriptRecord::UserInject { message_id, .. } => {
            if let Some(message_id) = message_id {
                *message_id = remap_identity(message_id, identities)?;
            }
        }
        TranscriptRecord::CompactSummary { compacted, .. } => {
            for message in compacted {
                if let Some(message_id) = &mut message.message_id {
                    *message_id = remap_identity(message_id, identities)?;
                }
            }
        }
        TranscriptRecord::MediaPlan { .. } => {}
    }
    Ok(())
}
