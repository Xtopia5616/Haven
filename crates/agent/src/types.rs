use std::collections::HashMap;

use haven_common::media::{
    MediaAssetSource, MediaInput, MediaInputStrategy, legacy_attachment_to_media_input,
};
use haven_common::text::sanitize_prompt_field;
use haven_common::types::{
    CanonicalMessage, CanonicalRole, CanonicalToolCall, ContentPart, InjectSource,
    MessageAttachment,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// One tool invocation within a [`ReActRound`] (parallel tools are siblings).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolRecord {
    pub action: Action,
    pub observation: Option<String>,
    #[serde(default)]
    pub action_index: u32,
    #[serde(default)]
    pub step_id: String,
}

/// One LLM step; parallel tools share `step_number` as siblings in `tools`
/// (no fake step inflation — Phase 8 / B2).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ReActRound {
    pub step_number: u32,
    pub thought: Option<String>,
    pub tools: Vec<ToolRecord>,
}

/// Append-only transcript record — sole snapshot authority (Phase 8 / B1-3).
/// Projected to canonical + [`ReActRound`] via [`project_transcript`].
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TranscriptRecord {
    Thought {
        step_number: u32,
        text: String,
        message_id: String,
    },
    /// Streamed/complete reasoning block projected to `messages` (type=`reasoning`).
    /// Not part of the LLM canonical transcript — lives in events for authority.
    Reasoning {
        step_number: u32,
        text: String,
        message_id: String,
    },
    ToolCall {
        step_number: u32,
        text: String,
        tool_calls: Vec<CanonicalToolCall>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reasoning: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        web_search_calls: Vec<Value>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        thinking_blocks: Vec<Value>,
    },
    ToolResult {
        step_number: u32,
        #[serde(default)]
        action_index: u32,
        #[serde(default)]
        step_id: String,
        canonical_observation: String,
        history_observation: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_call_id: Option<String>,
        action: Action,
    },
    UserInject {
        step_number: u32,
        source: InjectSource,
        /// Raw text — adapters prepend wire prefixes (Phase 8 / B3).
        text: String,
        /// Durable provider-neutral media metadata. Inline bytes are removed
        /// before this record is serialized into a snapshot.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        media_inputs: Vec<MediaInput>,
        /// Legacy snapshot field. It remains readable for the reset boundary,
        /// but new event records never serialize attachment bytes here.
        #[serde(default, skip_serializing)]
        attachments: Vec<MessageAttachment>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message_id: Option<String>,
    },
    /// Durable request-projection metadata. This is deliberately separate
    /// from the canonical message: a derived transcript/OCR part is still a
    /// media decision, not an opaque user sentence. The inputs are snapshot
    /// safe and retain the producer-owned asset ids across compaction/resume.
    MediaPlan {
        step_number: u32,
        strategy: MediaInputStrategy,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        media_inputs: Vec<MediaInput>,
        projections: Vec<haven_common::media::MediaProjection>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        notices: Vec<haven_common::media::MediaPlanNotice>,
    },
    CompactSummary {
        #[serde(serialize_with = "serialize_snapshot_canonical")]
        compacted: Vec<CanonicalMessage>,
        /// Snapshot-safe media metadata for raw parts represented by the
        /// compacted canonical root.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        media_inputs: Vec<MediaInput>,
        summary: String,
        tokens_before: u32,
        tokens_after: u32,
        episode_id: String,
        #[serde(default)]
        degraded: bool,
    },
}

/// Rollback point saved before tool execution (§2 / Phase 8 F4).
///
/// Haven currently implements rollback as an overwrite of the active timeline,
/// not as a user-visible branch tree. Stores only an index into the snapshot's
/// `events` — no Arc Vec copies.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BranchPoint {
    /// Index into parent `events` — restored state is `events[..event_cursor]`.
    pub event_cursor: usize,
    pub step_number: u32,
    /// `created_at` of the most recent session message at save time. On
    /// rollback, messages after this timestamp are deleted.
    #[serde(default)]
    pub last_msg_at: Option<String>,
}

/// Pending `ask` tool state persisted in the snapshot (Phase 4 / C5).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct AskPending {
    pub question: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub step_ids: Vec<String>,
}

/// One gated tool awaiting (or holding) a confirm decision (Phase 5 / E3).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ConfirmPendingTool {
    pub confirm_id: String,
    pub tool_name: String,
    pub tool_input: Value,
    #[serde(default)]
    pub tool_call_id: String,
    pub step_id: String,
    #[serde(default)]
    pub action_index: u32,
    pub risk_level: haven_common::types::RiskLevel,
    /// One-shot proof issued by the authorization engine for this exact
    /// invocation. `None` is used for safe/trusted siblings carried behind
    /// the same batch barrier and therefore rechecked normally on resume.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receipt: Option<haven_tools::ConfirmationReceipt>,
    /// `None` = still waiting; `Some(true/false)` = user decided.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decision: Option<bool>,
}

/// Pending safety-confirm batch persisted in the snapshot (Phase 5 / E3).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct ConfirmPending {
    pub step_number: u32,
    #[serde(default)]
    pub tools: Vec<ConfirmPendingTool>,
}

impl ConfirmPending {
    pub fn all_decided(&self) -> bool {
        !self.tools.is_empty() && self.tools.iter().all(|t| t.decision.is_some())
    }

    pub fn any_approved(&self) -> bool {
        self.tools.iter().any(|t| t.decision == Some(true))
    }
}

/// Per-run step budget recorded on the snapshot for observability (R4 / J1).
///
/// Storage is diagnostic only — the live loop still reads `max_steps` /
/// `session_max_steps` from the engine. Resume grants another full per-run
/// budget; `session_max_steps` (when set) caps absolute `step_number`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct RunBudget {
    /// First step number this run will execute (`start_step`).
    pub start_step: u32,
    /// Inclusive last step this run may reach.
    pub effective_max: u32,
    /// Configured per-run `max_steps` at run start.
    pub max_steps: u32,
    /// Optional session-lifetime absolute step cap.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_max_steps: Option<u32>,
}

/// Serializable snapshot of the ReAct loop state for pause/resume.
///
/// **Authority (Phase 8 / B1-3):** [`Self::events`] is the sole transcript.
/// Canonical and [`ReActRound`]s are derived via [`project_transcript`] /
/// [`Self::project`].
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct ReActSnapshot {
    pub events: Vec<TranscriptRecord>,
    pub step_number: u32,
    /// Rollback points keyed by step number for overwrite rollback (§2).
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub branch_points: HashMap<u32, BranchPoint>,
    /// Legacy wall-clock checkpoint retained for backward compatibility with
    /// old snapshots. New resume logic uses `last_ingress_seq`.
    #[serde(default)]
    pub saved_at: Option<String>,
    /// Highest durable message ingress sequence included when this snapshot
    /// was written. Resume recovers rows strictly after this cursor.
    #[serde(default)]
    pub last_ingress_seq: Option<i64>,
    /// Present only when the ReAct loop itself recorded a failed LLM stream.
    /// Continue may then use this step's branch point to replace the failed
    /// attempt. A normal periodic snapshot leaves this `None`, so an app or
    /// process interruption cannot truncate later completed history.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_partial_message_ids: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub awaiting_answer: Option<AskPending>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub awaiting_confirm: Option<ConfirmPending>,
    /// Last run's effective step budget (R4). Absent before a run starts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_budget: Option<RunBudget>,
}

impl ReActSnapshot {
    /// Parse the current events-authority snapshot shape.
    ///
    /// Snapshot upgrades are deliberately unsupported: a snapshot without
    /// `events` belongs to an incompatible Haven version and must be reset.
    pub fn from_json(json: &str) -> anyhow::Result<Self> {
        let mut snapshot: Self = serde_json::from_str(json)
            .map_err(|e| anyhow::anyhow!("corrupt or incompatible react_state: {e}"))?;
        // Older snapshots stored attachments directly on UserInject. Convert
        // them once at the read boundary so a later checkpoint cannot write
        // their inline bytes back out. The legacy field is intentionally
        // cleared after conversion; the new media_inputs field is the only
        // durable representation for subsequent saves.
        for event in &mut snapshot.events {
            if let TranscriptRecord::UserInject {
                media_inputs,
                attachments,
                ..
            } = event
            {
                if media_inputs.is_empty() && !attachments.is_empty() {
                    *media_inputs = attachment_media_inputs_for_snapshot(attachments);
                }
                attachments.clear();
            }
        }
        if snapshot.events.iter().any(|event| match event {
            TranscriptRecord::UserInject { text, .. } => text.starts_with("[conversation] "),
            TranscriptRecord::CompactSummary { compacted, .. } => compacted.iter().any(|message| {
                message.role == CanonicalRole::User
                    && message.content.iter().any(|part| {
                        matches!(part, ContentPart::Text(text) if text.starts_with("[conversation] "))
                    })
            }),
            _ => false,
        }) {
            anyhow::bail!(
                "corrupt or incompatible react_state: legacy conversation seed is unsupported"
            );
        }
        Ok(snapshot)
    }

    /// Project the full event log to canonical + rounds.
    pub fn project(&self) -> (Vec<CanonicalMessage>, Vec<ReActRound>) {
        project_transcript(&self.events)
    }

    /// Project with the current provider-facing media input policy.
    pub fn project_with_strategy(
        &self,
        strategy: MediaInputStrategy,
    ) -> (Vec<CanonicalMessage>, Vec<ReActRound>) {
        project_transcript_with_strategy(&self.events, strategy)
    }

    /// Project `events[..cursor]` (cursor clamped to `events.len()`).
    pub fn project_at(&self, cursor: usize) -> (Vec<CanonicalMessage>, Vec<ReActRound>) {
        let end = cursor.min(self.events.len());
        project_transcript(&self.events[..end])
    }

    /// Project a bounded event prefix with the current media policy.
    pub fn project_at_with_strategy(
        &self,
        cursor: usize,
        strategy: MediaInputStrategy,
    ) -> (Vec<CanonicalMessage>, Vec<ReActRound>) {
        let end = cursor.min(self.events.len());
        project_transcript_with_strategy(&self.events[..end], strategy)
    }
}

/// Project an append-only event log into the LLM transcript and debug/tool
/// rounds. Pure — no I/O. Parallel `ToolResult`s with the same `step_number`
/// become siblings on one [`ReActRound`].
pub fn project_transcript(events: &[TranscriptRecord]) -> (Vec<CanonicalMessage>, Vec<ReActRound>) {
    project_transcript_with_strategy(events, MediaInputStrategy::Auto)
}

/// Project an append-only event log using an explicit media input policy.
/// This remains pure so resume and live apply share the same attachment
/// selection semantics.
pub fn project_transcript_with_strategy(
    events: &[TranscriptRecord],
    strategy: MediaInputStrategy,
) -> (Vec<CanonicalMessage>, Vec<ReActRound>) {
    let mut canonical: Vec<CanonicalMessage> = Vec::new();
    let mut rounds: Vec<ReActRound> = Vec::new();

    for ev in events {
        match ev {
            TranscriptRecord::Thought {
                step_number, text, ..
            } => {
                rounds.push(ReActRound {
                    step_number: *step_number,
                    thought: Some(text.clone()),
                    tools: Vec::new(),
                });
            }
            TranscriptRecord::Reasoning { .. } => {
                // Chat projection only — not part of LLM canonical / rounds.
            }
            TranscriptRecord::ToolCall {
                text,
                tool_calls,
                reasoning,
                web_search_calls,
                thinking_blocks,
                ..
            } => {
                canonical.push(CanonicalMessage::assistant(
                    vec![ContentPart::text(text.clone())],
                    if tool_calls.is_empty() {
                        None
                    } else {
                        Some(tool_calls.clone())
                    },
                    reasoning.clone(),
                    web_search_calls.clone(),
                    thinking_blocks.clone(),
                ));
            }
            TranscriptRecord::ToolResult {
                step_number,
                action_index,
                step_id,
                canonical_observation,
                history_observation,
                tool_call_id,
                action,
            } => {
                // final_answer is rounds-only (mirrors pre-B1 history mutation;
                // the assistant text is pushed separately via finish_turn_end).
                let is_final = action.is_final || action.tool_name == "final_answer";
                if !is_final {
                    canonical.push(CanonicalMessage::tool(
                        vec![ContentPart::text(canonical_observation.clone())],
                        tool_call_id.clone(),
                    ));
                }
                if let Some(round) = rounds
                    .iter_mut()
                    .rev()
                    .find(|r| r.step_number == *step_number)
                {
                    round.tools.push(ToolRecord {
                        action: action.clone(),
                        observation: Some(history_observation.clone()),
                        action_index: *action_index,
                        step_id: step_id.clone(),
                    });
                } else {
                    rounds.push(ReActRound {
                        step_number: *step_number,
                        thought: None,
                        tools: vec![ToolRecord {
                            action: action.clone(),
                            observation: Some(history_observation.clone()),
                            action_index: *action_index,
                            step_id: step_id.clone(),
                        }],
                    });
                }
            }
            TranscriptRecord::UserInject {
                source,
                text,
                media_inputs,
                attachments,
                ..
            } => {
                let mut content = vec![ContentPart::text(text.clone())];
                if media_inputs.is_empty() {
                    for attachment in attachments {
                        let input = legacy_attachment_to_media_input(attachment);
                        append_media_projection(&mut content, &input, strategy);
                    }
                } else {
                    for input in media_inputs {
                        append_media_projection(&mut content, input, strategy);
                    }
                }
                canonical.push(CanonicalMessage::user_with_source(content, *source));
            }
            TranscriptRecord::MediaPlan { .. } => {
                // Durable diagnostics only; media plans never become model
                // transcript text during replay.
            }
            TranscriptRecord::CompactSummary { compacted, .. } => {
                canonical = compacted.clone();
            }
        }
    }

    (canonical, rounds)
}

pub(crate) fn append_media_projection(
    content: &mut Vec<ContentPart>,
    input: &MediaInput,
    strategy: MediaInputStrategy,
) {
    let projected = crate::react::media_input_to_content_part_with_strategy(input, strategy);
    let representation = match &projected {
        ContentPart::Image { .. } => Some("raw_image"),
        ContentPart::Audio { .. } => Some("raw_audio"),
        ContentPart::Text(_) => {
            crate::react::media_plan_for_inputs(std::slice::from_ref(input), strategy)
                .projections
                .first()
                .map(|projection| match projection.representation {
                    haven_common::media::MediaRepresentationKind::Transcript => "transcript",
                    haven_common::media::MediaRepresentationKind::OcrText => "ocr_text",
                    haven_common::media::MediaRepresentationKind::ExtractedText => "extracted_text",
                    haven_common::media::MediaRepresentationKind::ImageDescription => {
                        "image_description"
                    }
                    haven_common::media::MediaRepresentationKind::DocumentPages => "document_pages",
                    haven_common::media::MediaRepresentationKind::TableData => "table_data",
                    kind => match kind {
                        haven_common::media::MediaRepresentationKind::ManagedFileRef => {
                            "managed_file_ref"
                        }
                        _ => "derived",
                    },
                })
        }
    };
    content.push(projected);

    // Raw provider parts intentionally do not carry host metadata. Keep the
    // stable asset handle visible in the same user request as a compact,
    // app-generated notice so a later derivation can use media(asset_id=...)
    // without guessing whether the image/audio was already projected.
    if let Some(representation) = representation
        && matches!(
            input.asset.source,
            MediaAssetSource::UserAttachment
                | MediaAssetSource::Generated
                | MediaAssetSource::ToolOutput
        )
    {
        let asset_id = sanitize_prompt_field(&input.asset.asset_id, 96);
        content.push(ContentPart::text(format!(
            "[media_plan: {asset_id} -> {representation}; this representation is already in the request; use media(asset_id={asset_id}) for another representation]"
        )));
    }
}

/// Remove inline media bytes from a canonical compaction root before it is
/// persisted in a snapshot. Canonical messages are still the hot request
/// projection, so the live state keeps the original image/audio parts; the
/// durable root gets a typed text marker instead of an unbounded base64 blob.
pub(crate) fn canonical_for_snapshot(messages: &[CanonicalMessage]) -> Vec<CanonicalMessage> {
    canonical_for_snapshot_with_media_inputs(messages, &[])
}

/// Snapshot a canonical root while retaining the opaque identity for each
/// raw media part. The live canonical projection intentionally has no asset
/// field because that is provider-neutral wire data; this helper is the
/// durable boundary where the association is restored into a safe marker.
pub(crate) fn canonical_for_snapshot_with_media_inputs(
    messages: &[CanonicalMessage],
    media_inputs: &[MediaInput],
) -> Vec<CanonicalMessage> {
    let mut used_inputs = std::collections::HashSet::new();
    messages
        .iter()
        .cloned()
        .map(|mut message| {
            message.content = message
                .content
                .into_iter()
                .filter_map(|part| match part {
                    ContentPart::Text(text) if text.starts_with("[media_plan: ") => None,
                    ContentPart::Text(text) => Some(ContentPart::Text(text)),
                    ContentPart::Image { media_type, .. } => {
                        Some(ContentPart::text(snapshot_media_marker(
                            "image",
                            &media_type,
                            find_snapshot_media_input(
                                media_inputs,
                                &mut used_inputs,
                                MediaModalityForPart::Image,
                            ),
                        )))
                    }
                    ContentPart::Audio { media_type, .. } => {
                        Some(ContentPart::text(snapshot_media_marker(
                            "audio",
                            &media_type,
                            find_snapshot_media_input(
                                media_inputs,
                                &mut used_inputs,
                                MediaModalityForPart::Audio,
                            ),
                        )))
                    }
                })
                .collect();
            message
        })
        .collect()
}

#[derive(Clone, Copy)]
enum MediaModalityForPart {
    Image,
    Audio,
}

fn find_snapshot_media_input<'a>(
    inputs: &'a [MediaInput],
    used: &mut std::collections::HashSet<usize>,
    modality: MediaModalityForPart,
) -> Option<&'a MediaInput> {
    inputs.iter().enumerate().find_map(|(index, input)| {
        if used.contains(&index) || !media_input_matches_modality(input, modality) {
            return None;
        }
        used.insert(index);
        Some(input)
    })
}

fn media_input_matches_modality(input: &MediaInput, modality: MediaModalityForPart) -> bool {
    let media_type_matches = match modality {
        MediaModalityForPart::Image => input.asset.media_type.starts_with("image/"),
        MediaModalityForPart::Audio => input.asset.media_type.starts_with("audio/"),
    };
    media_type_matches
        || input.representations.iter().any(|representation| {
            representation
                .representation
                .raw_modality()
                .is_some_and(|kind| {
                    matches!(
                        (modality, kind),
                        (
                            MediaModalityForPart::Image,
                            haven_common::media::MediaModality::Image
                        ) | (
                            MediaModalityForPart::Audio,
                            haven_common::media::MediaModality::Audio
                        )
                    )
                })
        })
}

fn snapshot_media_marker(label: &str, media_type: &str, input: Option<&MediaInput>) -> String {
    let media_type = sanitize_prompt_field(media_type, 96);
    match input {
        Some(input) => format!(
            "[managed {label} omitted from snapshot; asset_id={}; media_type={media_type}]",
            sanitize_prompt_field(&input.asset.asset_id, 96)
        ),
        None => format!("[managed {label} omitted from snapshot; media_type={media_type}]"),
    }
}

fn serialize_snapshot_canonical<S>(
    messages: &[CanonicalMessage],
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    canonical_for_snapshot(messages).serialize(serializer)
}

/// Convert a legacy attachment to the same request projection used by the
/// durable media event. Kept here so old snapshots and new snapshots share
/// exactly one projection implementation.
pub(crate) fn attachment_media_inputs_for_snapshot(
    attachments: &[MessageAttachment],
) -> Vec<MediaInput> {
    attachments
        .iter()
        .map(legacy_attachment_to_media_input)
        .map(|input| input.for_snapshot())
        .collect()
}

/// Collect the snapshot-safe media identities carried by the current event
/// root. A compaction boundary replaces older events, so encountering one
/// resets the collection just like transcript projection resets canonical
/// history.
pub(crate) fn media_inputs_from_events(events: &[TranscriptRecord]) -> Vec<MediaInput> {
    let mut inputs = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for event in events {
        let event_inputs: Vec<MediaInput> = match event {
            TranscriptRecord::CompactSummary { media_inputs, .. } => {
                inputs.clear();
                seen.clear();
                media_inputs.clone()
            }
            TranscriptRecord::UserInject {
                media_inputs,
                attachments,
                ..
            } => {
                if media_inputs.is_empty() {
                    attachments
                        .iter()
                        .map(legacy_attachment_to_media_input)
                        .collect()
                } else {
                    media_inputs.clone()
                }
            }
            TranscriptRecord::MediaPlan { media_inputs, .. } => media_inputs.clone(),
            _ => Vec::new(),
        };
        for input in event_inputs {
            if seen.insert(input.asset.asset_id.clone()) {
                inputs.push(input);
            }
        }
    }
    inputs
}

/// Test/helper: wrap a pre-built canonical list as a single CompactSummary
/// seed event so snapshots can be constructed without replaying applies.
pub fn seed_events_from_canonical(canonical: Vec<CanonicalMessage>) -> Vec<TranscriptRecord> {
    vec![TranscriptRecord::CompactSummary {
        compacted: canonical,
        media_inputs: Vec::new(),
        summary: String::new(),
        tokens_before: 0,
        tokens_after: 0,
        episode_id: haven_common::types::new_id("msg"),
        degraded: false,
    }]
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Action {
    pub tool_name: String,
    pub tool_input: Value,
    pub is_final: bool,
    pub tool_call_id: Option<String>,
}

/// Result of [`crate::AgentLayer::process_input`]. Carries the persisted
/// user-message id so the UI can replace its optimistic temp id with the
/// canonical `msg-*` without content/timestamp guessing.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum ProcessResult {
    SessionCreated {
        session_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message_id: Option<String>,
    },
    Supplemented {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message_id: Option<String>,
    },
}

impl ProcessResult {
    pub fn session_created(session_id: impl Into<String>, message_id: Option<String>) -> Self {
        Self::SessionCreated {
            session_id: session_id.into(),
            message_id,
        }
    }

    pub fn supplemented(message_id: Option<String>) -> Self {
        Self::Supplemented { message_id }
    }

    pub fn message_id(&self) -> Option<&str> {
        match self {
            Self::SessionCreated { message_id, .. } | Self::Supplemented { message_id } => {
                message_id.as_deref()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use haven_common::media::{
        MediaDerivation, MediaProvenance, MediaRepresentation, MediaRepresentationKind,
        MediaRepresentationPayload,
    };
    use haven_common::types::CanonicalRole;

    fn canonical_msg(role: CanonicalRole, text: &str) -> CanonicalMessage {
        CanonicalMessage {
            role,
            content: vec![ContentPart::text(text)],
            tool_calls: None,
            tool_call_id: None,
            reasoning: None,
            web_search_calls: Vec::new(),
            thinking_blocks: Vec::new(),
            source: None,
            id: None,
        }
    }

    #[test]
    fn action_serde_roundtrip() {
        let action = Action {
            tool_name: "files".into(),
            tool_input: serde_json::json!({"path": "C:/tmp/a.txt"}),
            is_final: false,
            tool_call_id: Some("call_1".into()),
        };
        let json = serde_json::to_string(&action).unwrap();
        let back: Action = serde_json::from_str(&json).unwrap();
        assert_eq!(back.tool_name, "files");
        assert_eq!(back.tool_input, serde_json::json!({"path": "C:/tmp/a.txt"}));
        assert!(!back.is_final);
        assert_eq!(back.tool_call_id.as_deref(), Some("call_1"));
    }

    #[test]
    fn action_missing_tool_call_id_defaults_to_none() {
        let json = r#"{"tool_name":"shell","tool_input":{"cmd":"dir"},"is_final":true}"#;
        let action: Action = serde_json::from_str(json).unwrap();
        assert!(action.is_final);
        assert_eq!(action.tool_call_id, None);
    }

    #[test]
    fn project_parallel_tools_one_round() {
        let events = vec![
            TranscriptRecord::Thought {
                step_number: 1,
                text: "run both".into(),
                message_id: "step-1".into(),
            },
            TranscriptRecord::ToolCall {
                step_number: 1,
                text: "calling".into(),
                tool_calls: vec![
                    CanonicalToolCall {
                        id: "c1".into(),
                        name: "a".into(),
                        arguments: serde_json::json!({}),
                    },
                    CanonicalToolCall {
                        id: "c2".into(),
                        name: "b".into(),
                        arguments: serde_json::json!({}),
                    },
                ],
                reasoning: None,
                web_search_calls: vec![],
                thinking_blocks: vec![],
            },
            TranscriptRecord::ToolResult {
                step_number: 1,
                action_index: 0,
                step_id: "step-a".into(),
                canonical_observation: "ra".into(),
                history_observation: "ra".into(),
                tool_call_id: Some("c1".into()),
                action: Action {
                    tool_name: "a".into(),
                    tool_input: serde_json::json!({}),
                    is_final: false,
                    tool_call_id: Some("c1".into()),
                },
            },
            TranscriptRecord::ToolResult {
                step_number: 1,
                action_index: 1,
                step_id: "step-b".into(),
                canonical_observation: "rb".into(),
                history_observation: "rb".into(),
                tool_call_id: Some("c2".into()),
                action: Action {
                    tool_name: "b".into(),
                    tool_input: serde_json::json!({}),
                    is_final: false,
                    tool_call_id: Some("c2".into()),
                },
            },
        ];
        let (canonical, rounds) = project_transcript(&events);
        assert_eq!(rounds.len(), 1, "parallel tools must share one round");
        assert_eq!(rounds[0].tools.len(), 2);
        assert_eq!(canonical.len(), 3); // assistant + 2 tool
    }

    #[test]
    fn branch_point_missing_last_msg_at_defaults_to_none() {
        let json = r#"{"event_cursor": 2, "step_number": 2}"#;
        let bp: BranchPoint = serde_json::from_str(json).unwrap();
        assert_eq!(bp.step_number, 2);
        assert_eq!(bp.event_cursor, 2);
        assert_eq!(bp.last_msg_at, None);
    }

    #[test]
    fn branch_point_roundtrip_with_last_msg_at() {
        let bp = BranchPoint {
            event_cursor: 4,
            step_number: 5,
            last_msg_at: Some("2026-07-31T12:00:00Z".into()),
        };
        let json = serde_json::to_string(&bp).unwrap();
        let back: BranchPoint = serde_json::from_str(&json).unwrap();
        assert_eq!(back.event_cursor, 4);
        assert_eq!(back.step_number, 5);
        assert_eq!(back.last_msg_at.as_deref(), Some("2026-07-31T12:00:00Z"));
    }

    #[test]
    fn snapshot_roundtrip_with_branch_points() {
        let mut snapshot = ReActSnapshot {
            events: seed_events_from_canonical(vec![canonical_msg(CanonicalRole::System, "sys")]),
            step_number: 7,
            ..Default::default()
        };
        snapshot.branch_points.insert(
            4,
            BranchPoint {
                event_cursor: 1,
                step_number: 4,
                last_msg_at: None,
            },
        );
        let json = serde_json::to_string(&snapshot).unwrap();
        assert!(json.contains("branch_points"));
        assert!(json.contains("events"));
        assert!(!json.contains("\"canonical\""));
        assert!(!json.contains("\"history\""));
        let back = ReActSnapshot::from_json(&json).unwrap();
        assert_eq!(back.step_number, 7);
        assert_eq!(back.branch_points.len(), 1);
        assert_eq!(back.branch_points.get(&4).unwrap().event_cursor, 1);
        let (canonical, _) = back.project();
        assert_eq!(canonical.len(), 1);
    }

    #[test]
    fn new_user_inject_snapshot_contains_media_metadata_not_inline_bytes() {
        let mut attachment = MessageAttachment::new("image/png", "aGVsbG8=");
        attachment.asset_id = Some("asset-0123456789abcdef0123456789abcdef".into());
        attachment.filename = Some("photo.png".into());
        attachment.path = Some(r"C:\haven\uploads\photo.png".into());
        attachment
            .representations
            .push(MediaRepresentation::available(
                MediaRepresentationKind::OcrText,
                MediaProvenance::Derived {
                    operation: MediaDerivation::Ocr,
                    provider: Some("ocr".into()),
                    source_kind: Some(MediaRepresentationKind::RawImage),
                },
                MediaRepresentationPayload::Text("recognized text".into()),
            ));

        let record = TranscriptRecord::UserInject {
            step_number: 1,
            source: InjectSource::FollowUp,
            text: "请看图".into(),
            media_inputs: attachment_media_inputs_for_snapshot(&[attachment]),
            attachments: Vec::new(),
            message_id: Some("msg-0123456789abcdef0123456789abcdef".into()),
        };
        let json = serde_json::to_string(&record).unwrap();

        assert!(!json.contains("aGVsbG8="));
        assert!(!json.contains(r"C:\\haven\\uploads"));
        assert!(json.contains("managed_file_ref"));
        assert!(json.contains("recognized text"));
        assert!(json.contains("ocr_text"));
    }

    #[test]
    fn compact_summary_snapshot_replaces_inline_media_with_safe_marker() {
        let snapshot = ReActSnapshot {
            events: seed_events_from_canonical(vec![CanonicalMessage {
                role: CanonicalRole::User,
                content: vec![ContentPart::Image {
                    content_type: "image_url".into(),
                    media_type: "image/png".into(),
                    data: "aGVsbG8=".into(),
                }],
                tool_calls: None,
                tool_call_id: None,
                reasoning: None,
                web_search_calls: Vec::new(),
                thinking_blocks: Vec::new(),
                source: None,
                id: None,
            }]),
            ..Default::default()
        };
        let json = serde_json::to_string(&snapshot).unwrap();

        assert!(!json.contains("aGVsbG8="));
        assert!(json.contains("managed image omitted from snapshot"));
        assert!(json.contains("image/png"));
    }

    #[test]
    fn compact_summary_marker_keeps_asset_identity_when_media_metadata_is_present() {
        let mut attachment = MessageAttachment::new("image/png", "aGVsbG8=");
        attachment.asset_id = Some("asset-0123456789abcdef0123456789abcdef".into());
        attachment.path = Some(r"C:\haven\uploads\photo.png".into());
        let input = legacy_attachment_to_media_input(&attachment);
        let messages = vec![CanonicalMessage {
            role: CanonicalRole::User,
            content: vec![ContentPart::Image {
                content_type: "image_url".into(),
                media_type: "image/png".into(),
                data: "aGVsbG8=".into(),
            }],
            tool_calls: None,
            tool_call_id: None,
            reasoning: None,
            web_search_calls: Vec::new(),
            thinking_blocks: Vec::new(),
            source: None,
            id: None,
        }];
        let snapshot = canonical_for_snapshot_with_media_inputs(&messages, &[input]);
        let marker = match &snapshot[0].content[0] {
            ContentPart::Text(text) => text,
            other => panic!("expected snapshot marker, got {other:?}"),
        };
        assert!(marker.contains("asset_id=asset-0123456789abcdef0123456789abcdef"));
        assert!(!marker.contains("aGVsbG8="));
    }

    #[test]
    fn media_plan_is_a_structured_durable_event() {
        let record = TranscriptRecord::MediaPlan {
            step_number: 3,
            strategy: MediaInputStrategy::ExtractedPreferred,
            media_inputs: Vec::new(),
            projections: Vec::new(),
            notices: Vec::new(),
        };
        let json = serde_json::to_string(&record).unwrap();
        assert!(json.contains("\"type\":\"media_plan\""));
        assert!(json.contains("extracted_preferred"));
        let roundtrip: TranscriptRecord = serde_json::from_str(&json).unwrap();
        assert!(matches!(roundtrip, TranscriptRecord::MediaPlan { .. }));
    }

    #[test]
    fn reading_legacy_attachment_snapshot_migrates_without_reserializing_bytes() {
        let json = serde_json::json!({
            "events": [{
                "type": "user_inject",
                "step_number": 1,
                "source": "follow_up",
                "text": "请看图",
                "attachments": [{
                    "media_type": "image/png",
                    "data": "aGVsbG8="
                }]
            }],
            "step_number": 1
        })
        .to_string();

        let snapshot = ReActSnapshot::from_json(&json).unwrap();
        let saved = serde_json::to_string(&snapshot).unwrap();

        assert!(!saved.contains("aGVsbG8="));
        assert!(saved.contains("managed_file_ref"));
        assert!(matches!(
            &snapshot.events[0],
            TranscriptRecord::UserInject {
                media_inputs,
                attachments,
                ..
            } if media_inputs.len() == 1 && attachments.is_empty()
        ));
    }

    #[test]
    fn snapshot_from_json_rejects_legacy_phase7_shape() {
        // ContentPart::Text is an untagged string on the wire.
        let legacy = serde_json::json!({
            "canonical": [
                {
                    "role": "user",
                    "content": ["hi"]
                }
            ],
            "history": [],
            "step_number": 3,
            "branch_points": {
                "2": {
                    "canonical": [
                        {
                            "role": "user",
                            "content": ["hi"]
                        }
                    ],
                    "step_number": 2,
                    "last_msg_at": "2026-08-01T00:00:00Z"
                }
            },
            "saved_at": "2026-08-01T00:01:00Z"
        });
        let err = ReActSnapshot::from_json(&legacy.to_string()).unwrap_err();
        assert!(err.to_string().contains("incompatible"));
    }

    #[test]
    fn snapshot_empty_branch_points_skipped_in_json() {
        let snapshot = ReActSnapshot {
            events: vec![],
            step_number: 1,
            ..Default::default()
        };
        let json = serde_json::to_string(&snapshot).unwrap();
        assert!(!json.contains("branch_points"));
        assert!(!json.contains("awaiting_answer"));
        let back: ReActSnapshot = serde_json::from_str(&json).unwrap();
        assert!(back.branch_points.is_empty());
        assert!(back.awaiting_answer.is_none());
    }

    #[test]
    fn snapshot_awaiting_answer_roundtrip() {
        let snapshot = ReActSnapshot {
            events: vec![],
            step_number: 2,
            awaiting_answer: Some(AskPending {
                question: "which file?".into(),
                step_ids: vec!["step-abc".into()],
            }),
            ..Default::default()
        };
        let json = serde_json::to_string(&snapshot).unwrap();
        let back: ReActSnapshot = serde_json::from_str(&json).unwrap();
        let pending = back.awaiting_answer.expect("flag restored");
        assert_eq!(pending.question, "which file?");
        assert_eq!(pending.step_ids, vec!["step-abc".to_string()]);
    }

    #[test]
    fn snapshot_awaiting_confirm_roundtrip_preserves_invocation_identity() {
        let snapshot = ReActSnapshot {
            events: vec![],
            step_number: 4,
            awaiting_confirm: Some(ConfirmPending {
                step_number: 4,
                tools: vec![
                    ConfirmPendingTool {
                        confirm_id: "conf-a".into(),
                        tool_name: "run_command".into(),
                        tool_input: serde_json::json!({"command":"same"}),
                        tool_call_id: "call-a".into(),
                        step_id: "step-a".into(),
                        action_index: 0,
                        risk_level: haven_common::types::RiskLevel::High,
                        receipt: None,
                        decision: None,
                    },
                    ConfirmPendingTool {
                        confirm_id: "conf-b".into(),
                        tool_name: "run_command".into(),
                        tool_input: serde_json::json!({"command":"same"}),
                        tool_call_id: "call-b".into(),
                        step_id: "step-a".into(),
                        action_index: 1,
                        risk_level: haven_common::types::RiskLevel::High,
                        receipt: None,
                        decision: None,
                    },
                ],
            }),
            ..Default::default()
        };
        let json = serde_json::to_string(&snapshot).unwrap();
        let back: ReActSnapshot = serde_json::from_str(&json).unwrap();
        let tools = back.awaiting_confirm.unwrap().tools;
        assert_eq!(tools[0].step_id, "step-a");
        assert_eq!(tools[0].action_index, 0);
        assert_eq!(tools[0].tool_call_id, "call-a");
        assert_eq!(tools[1].step_id, "step-a");
        assert_eq!(tools[1].action_index, 1);
        assert_eq!(tools[1].tool_call_id, "call-b");
    }

    #[test]
    fn snapshot_run_budget_roundtrip() {
        let snapshot = ReActSnapshot {
            events: vec![],
            step_number: 3,
            run_budget: Some(RunBudget {
                start_step: 1,
                effective_max: 20,
                max_steps: 20,
                session_max_steps: Some(100),
            }),
            ..Default::default()
        };
        let json = serde_json::to_string(&snapshot).unwrap();
        assert!(json.contains("run_budget"));
        let back: ReActSnapshot = serde_json::from_str(&json).unwrap();
        let budget = back.run_budget.expect("budget restored");
        assert_eq!(budget.start_step, 1);
        assert_eq!(budget.effective_max, 20);
        assert_eq!(budget.max_steps, 20);
        assert_eq!(budget.session_max_steps, Some(100));
    }

    #[test]
    fn snapshot_project_at_truncates() {
        let events = vec![
            TranscriptRecord::UserInject {
                step_number: 1,
                source: InjectSource::FollowUp,
                text: "a".into(),
                media_inputs: vec![],
                attachments: vec![],
                message_id: None,
            },
            TranscriptRecord::UserInject {
                step_number: 1,
                source: InjectSource::FollowUp,
                text: "b".into(),
                media_inputs: vec![],
                attachments: vec![],
                message_id: None,
            },
        ];
        let snapshot = ReActSnapshot {
            events,
            step_number: 1,
            ..Default::default()
        };
        let (full, _) = snapshot.project();
        assert_eq!(full.len(), 2);
        let (at1, _) = snapshot.project_at(1);
        assert_eq!(at1.len(), 1);
    }

    #[test]
    fn process_result_variants_roundtrip() {
        for result in [
            ProcessResult::session_created("ses-1", Some("msg-abc".into())),
            ProcessResult::session_created("ses-2", None),
            ProcessResult::supplemented(Some("msg-def".into())),
            ProcessResult::supplemented(None),
        ] {
            let json = serde_json::to_string(&result).unwrap();
            let back: ProcessResult = serde_json::from_str(&json).unwrap();
            assert_eq!(back, result);
        }
    }
}
