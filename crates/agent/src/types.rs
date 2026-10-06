use haven_common::media::{MediaAssetSource, MediaInput, MediaInputStrategy};
use haven_common::text::sanitize_prompt_field;
use haven_common::types::{CanonicalMessage, CanonicalToolCall, ContentPart, InjectSource};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// One tool invocation within a [`ReActRound`] (parallel tools are siblings).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolRecord {
    pub tool_call: ToolCall,
    pub observation: Option<String>,
    pub tool_index: u32,
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

/// Append-only transcript record — payload stored in `session_events`.
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
        tool_index: u32,
        step_id: String,
        canonical_observation: String,
        history_observation: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        tool_call_id: Option<String>,
        tool_call: ToolCall,
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
        #[serde(skip_serializing_if = "Option::is_none")]
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
        /// Step at which compaction replaced the active transcript.
        step_number: u32,
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
    /// Index into the active transcript cache — restored state is
    /// `events[..event_cursor]`. The durable rollback cursor is the corresponding
    /// `session_events.sequence` stored by the event store marker.
    pub event_cursor: usize,
    pub step_number: u32,
    /// `created_at` of the most recent session message at save time. On
    /// rollback, messages after this timestamp are deleted.
    pub last_msg_at: Option<String>,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_max_steps: Option<u32>,
}

/// Read-only views derived together from the durable transcript event log.
#[derive(Debug, Clone)]
pub struct TranscriptProjection {
    /// Provider-neutral conversation passed to the model.
    pub canonical_messages: Vec<CanonicalMessage>,
    /// Agent step history used by resume and per-session tool restoration.
    pub react_rounds: Vec<ReActRound>,
}

pub fn project_transcript(events: &[TranscriptRecord]) -> TranscriptProjection {
    project_transcript_with_strategy(events, MediaInputStrategy::Auto)
}

/// Project an append-only event log using an explicit media input policy.
/// This remains pure so resume and live apply share the same attachment
/// selection semantics.
pub fn project_transcript_with_strategy(
    events: &[TranscriptRecord],
    strategy: MediaInputStrategy,
) -> TranscriptProjection {
    let mut canonical: Vec<CanonicalMessage> = Vec::new();
    let mut rounds: Vec<ReActRound> = Vec::new();
    let mut pending_tool_results = Vec::new();

    for ev in events {
        if matches!(ev, TranscriptRecord::ToolResult { .. }) {
            pending_tool_results.push(ev);
            continue;
        }
        project_pending_tool_results(&mut pending_tool_results, &mut canonical, &mut rounds);
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
            TranscriptRecord::ToolResult { .. } => unreachable!("results were deferred above"),
            TranscriptRecord::UserInject {
                source,
                text,
                media_inputs,
                ..
            } => {
                let mut content = vec![ContentPart::text(text.clone())];
                for input in media_inputs {
                    append_media_projection(&mut content, input, strategy);
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

    project_pending_tool_results(&mut pending_tool_results, &mut canonical, &mut rounds);
    for round in &mut rounds {
        round.tools.sort_by_key(|tool| tool.tool_index);
    }

    TranscriptProjection {
        canonical_messages: canonical,
        react_rounds: rounds,
    }
}

fn project_pending_tool_results(
    pending: &mut Vec<&TranscriptRecord>,
    canonical: &mut Vec<CanonicalMessage>,
    rounds: &mut Vec<ReActRound>,
) {
    pending.sort_by_key(|record| match record {
        TranscriptRecord::ToolResult {
            step_number,
            tool_index,
            ..
        } => (*step_number, *tool_index),
        _ => unreachable!("pending tool results only contain ToolResult records"),
    });
    for record in pending.drain(..) {
        let TranscriptRecord::ToolResult {
            step_number,
            tool_index,
            step_id,
            canonical_observation,
            history_observation,
            tool_call_id,
            tool_call,
        } = record
        else {
            unreachable!("pending tool results only contain ToolResult records")
        };
        // final_answer is rounds-only (mirrors pre-B1 history mutation; the
        // assistant text is pushed separately via finish_turn_end).
        if !tool_call.is_final && tool_call.tool_name != "final_answer" {
            canonical.push(CanonicalMessage::tool(
                vec![ContentPart::text(canonical_observation.clone())],
                tool_call_id.clone(),
            ));
        }
        if let Some(round) = rounds
            .iter_mut()
            .rev()
            .find(|round| round.step_number == *step_number)
        {
            round.tools.push(ToolRecord {
                tool_call: tool_call.clone(),
                observation: Some(history_observation.clone()),
                tool_index: *tool_index,
                step_id: step_id.clone(),
            });
        } else {
            rounds.push(ReActRound {
                step_number: *step_number,
                thought: None,
                tools: vec![ToolRecord {
                    tool_call: tool_call.clone(),
                    observation: Some(history_observation.clone()),
                    tool_index: *tool_index,
                    step_id: step_id.clone(),
                }],
            });
        }
    }
}

pub(crate) fn append_media_projection(
    content: &mut Vec<ContentPart>,
    input: &MediaInput,
    strategy: MediaInputStrategy,
) {
    let projected = crate::react::media_input_to_content_part_with_strategy(input, strategy);
    let representation = match &projected {
        ContentPart::Image { .. } => {
            Some(haven_common::media::MediaRepresentationKind::RawImage.as_str())
        }
        ContentPart::Audio { .. } => {
            Some(haven_common::media::MediaRepresentationKind::RawAudio.as_str())
        }
        ContentPart::Video { .. } => {
            Some(haven_common::media::MediaRepresentationKind::RawVideo.as_str())
        }
        ContentPart::Text(_) => {
            crate::react::media_plan_for_inputs(std::slice::from_ref(input), strategy)
                .projections
                .first()
                .map(|projection| projection.representation.as_str())
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
            "[media_plan: asset_id={asset_id} -> {representation}; this representation is already in the request; prefer reading asset_id from the previous tool result; use media(asset_id={asset_id}) for another representation]"
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
                    ContentPart::Video { media_type, .. } => {
                        Some(ContentPart::text(snapshot_media_marker(
                            "video",
                            &media_type,
                            find_snapshot_media_input(
                                media_inputs,
                                &mut used_inputs,
                                MediaModalityForPart::Video,
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
    Video,
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
        MediaModalityForPart::Video => input.asset.media_type.starts_with("video/"),
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
                        ) | (
                            MediaModalityForPart::Video,
                            haven_common::media::MediaModality::Video,
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
            TranscriptRecord::UserInject { media_inputs, .. } => media_inputs.clone(),
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
        step_number: 1,
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
pub struct ToolCall {
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
    use haven_common::media::message_attachment_to_media_input;
    use haven_common::media::{
        MediaDerivation, MediaProvenance, MediaRepresentation, MediaRepresentationKind,
        MediaRepresentationPayload,
    };
    use haven_common::types::CanonicalRole;
    use haven_common::types::MessageAttachment;

    #[test]
    fn tool_call_serde_roundtrip() {
        let tool_call = ToolCall {
            tool_name: "files".into(),
            tool_input: serde_json::json!({"path": "C:/tmp/a.txt"}),
            is_final: false,
            tool_call_id: Some("call_1".into()),
        };
        let json = serde_json::to_string(&tool_call).unwrap();
        let back: ToolCall = serde_json::from_str(&json).unwrap();
        assert_eq!(back.tool_name, "files");
        assert_eq!(back.tool_input, serde_json::json!({"path": "C:/tmp/a.txt"}));
        assert!(!back.is_final);
        assert_eq!(back.tool_call_id.as_deref(), Some("call_1"));
    }

    #[test]
    fn tool_call_missing_tool_call_id_defaults_to_none() {
        let json = r#"{"tool_name":"shell","tool_input":{"cmd":"dir"},"is_final":true}"#;
        let tool_call: ToolCall = serde_json::from_str(json).unwrap();
        assert!(tool_call.is_final);
        assert_eq!(tool_call.tool_call_id, None);
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
                tool_index: 1,
                step_id: "step-b".into(),
                canonical_observation: "rb".into(),
                history_observation: "rb".into(),
                tool_call_id: Some("c2".into()),
                tool_call: ToolCall {
                    tool_name: "b".into(),
                    tool_input: serde_json::json!({}),
                    is_final: false,
                    tool_call_id: Some("c2".into()),
                },
            },
            TranscriptRecord::ToolResult {
                step_number: 1,
                tool_index: 0,
                step_id: "step-a".into(),
                canonical_observation: "ra".into(),
                history_observation: "ra".into(),
                tool_call_id: Some("c1".into()),
                tool_call: ToolCall {
                    tool_name: "a".into(),
                    tool_input: serde_json::json!({}),
                    is_final: false,
                    tool_call_id: Some("c1".into()),
                },
            },
        ];
        let projection = project_transcript(&events);
        let canonical = projection.canonical_messages;
        let rounds = projection.react_rounds;
        assert_eq!(rounds.len(), 1, "parallel tools must share one round");
        assert_eq!(rounds[0].tools.len(), 2);
        assert_eq!(rounds[0].tools[0].tool_index, 0);
        assert_eq!(rounds[0].tools[0].step_id, "step-a");
        assert_eq!(rounds[0].tools[1].tool_index, 1);
        assert_eq!(rounds[0].tools[1].step_id, "step-b");
        assert_eq!(canonical.len(), 3); // assistant + 2 tool
        assert_eq!(canonical[1].tool_call_id.as_deref(), Some("c1"));
        assert_eq!(canonical[2].tool_call_id.as_deref(), Some("c2"));
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
            media_inputs: vec![message_attachment_to_media_input(&attachment).for_snapshot()],
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
    fn compact_summary_event_replaces_inline_media_with_safe_marker() {
        let events = seed_events_from_canonical(vec![CanonicalMessage {
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
        }]);
        let json = serde_json::to_string(&events).unwrap();

        assert!(!json.contains("aGVsbG8="));
        assert!(json.contains("managed image omitted from snapshot"));
        assert!(json.contains("image/png"));
    }

    #[test]
    fn compact_summary_marker_keeps_asset_identity_when_media_metadata_is_present() {
        let mut attachment = MessageAttachment::new("image/png", "aGVsbG8=");
        attachment.asset_id = Some("asset-0123456789abcdef0123456789abcdef".into());
        attachment.path = Some(r"C:\haven\uploads\photo.png".into());
        let input = message_attachment_to_media_input(&attachment);
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
    fn project_transcript_prefix_truncates() {
        let events = vec![
            TranscriptRecord::UserInject {
                step_number: 1,
                source: InjectSource::FollowUp,
                text: "a".into(),
                media_inputs: vec![],
                message_id: None,
            },
            TranscriptRecord::UserInject {
                step_number: 1,
                source: InjectSource::FollowUp,
                text: "b".into(),
                media_inputs: vec![],
                message_id: None,
            },
        ];
        let full = project_transcript(&events).canonical_messages;
        assert_eq!(full.len(), 2);
        let prefix = project_transcript(&events[..1]).canonical_messages;
        assert_eq!(prefix.len(), 1);
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
