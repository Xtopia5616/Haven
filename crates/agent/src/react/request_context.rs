//! Immutable provider-request context for one ReAct turn.
//!
//! The run state is durable transcript state. A provider request is an
//! ephemeral projection of that state: it may contain a retry hint, a
//! cut-off instruction, or repairs required by a provider's tool-call
//! contract, but none of those changes belong in the transcript. Keeping this
//! projection in one type makes it impossible for Turn, retry, and compaction
//! paths to each invent their own clone/append/sanitize sequence.

use super::{MediaRequirements, ReActEngine, ReActState, RetryNudge, canonical_media_requirements};
use crate::types::TranscriptRecord;
use haven_common::media::{
    CapabilityProfile, MediaInput, MediaInputStrategy, MediaModality, MediaPlan,
    MediaProjectionMode, build_media_plan, legacy_attachment_to_media_input,
};
use haven_common::types::{CanonicalMessage, ContentPart, MessageAttachment};

/// One immutable provider request snapshot.
#[derive(Debug, Clone)]
pub(crate) struct RequestContext {
    messages: Vec<CanonicalMessage>,
    /// Durable media metadata aligned to `messages[].content[]`. Provider
    /// `ContentPart` deliberately contains only wire-safe bytes, so planning
    /// from it alone would mint synthetic asset ids and break the producer →
    /// asset → consumer identity contract.
    media_inputs: Vec<Vec<Option<MediaInput>>>,
    repairs: usize,
}

impl RequestContext {
    /// Build the provider view from the current durable projection.
    ///
    /// Sanitization is deliberately performed exactly here, at the provider
    /// boundary. The repaired copy is never written back to `ReActState`.
    pub(super) fn from_state(state: &ReActState, retry_nudge: Option<&RetryNudge>) -> Self {
        let mut messages = state.canonical.clone();
        if let Some(nudge) = retry_nudge {
            ReActEngine::attach_failure_nudge(
                &mut messages,
                &nudge.text,
                Some(&nudge.tool_call_id),
            );
        }
        let media_inputs = media_inputs_for_state(state, &messages);
        Self::from_messages(messages, media_inputs)
    }

    /// Build a new request view with a trailing, provider-only user
    /// instruction. Used by the cut-off retry. The original snapshot remains
    /// untouched, so retries cannot accidentally accumulate instructions.
    pub(super) fn with_user_instruction(&self, instruction: impl Into<String>) -> Self {
        let mut messages = self.messages.clone();
        let mut media_inputs = self.media_inputs.clone();
        messages.push(CanonicalMessage::user_text(instruction));
        media_inputs.push(vec![None]);
        Self::from_messages(messages, media_inputs)
    }

    /// Re-project raw media against the selected adapter's actual wire
    /// capabilities. Durable canonical state stays provider-neutral; this
    /// request-only copy may replace an unsafe raw part with an explicit safe
    /// text fallback and returns stable diagnostics for the UI/log.
    pub(super) fn with_capabilities(
        &self,
        capabilities: &CapabilityProfile,
        strategy: MediaInputStrategy,
    ) -> (Self, MediaPlan) {
        let mut messages = self.messages.clone();
        let mut planned_inputs = Vec::new();
        let mut media_positions = Vec::new();
        for (message_index, message) in messages.iter().enumerate() {
            for (part_index, part) in message.content.iter().enumerate() {
                let Some(fallback) = attachment_from_content_part(part) else {
                    continue;
                };
                let input = self
                    .media_inputs
                    .get(message_index)
                    .and_then(|parts| parts.get(part_index))
                    .and_then(Option::as_ref)
                    .cloned()
                    .unwrap_or_else(|| legacy_attachment_to_media_input(&fallback));
                media_positions.push((message_index, part_index, input.asset.asset_id.clone()));
                planned_inputs.push(input);
            }
        }

        let plan = build_media_plan(&planned_inputs, capabilities, strategy);
        let projected = haven_llm::media::project_media_plan(&plan, &planned_inputs).ok();
        let projected_by_asset = projected
            .into_iter()
            .flatten()
            .zip(plan.projections.iter())
            .map(|(part, projection)| (projection.asset_id.clone(), part))
            .collect::<std::collections::HashMap<_, _>>();
        let asset_ids_by_position = media_positions
            .into_iter()
            .map(|(message_index, part_index, asset_id)| ((message_index, part_index), asset_id))
            .collect::<std::collections::HashMap<_, _>>();

        for (message_index, message) in messages.iter_mut().enumerate() {
            let original = std::mem::take(&mut message.content);
            let mut content = Vec::with_capacity(original.len());
            for (part_index, part) in original.into_iter().enumerate() {
                let Some(asset_id) = asset_ids_by_position.get(&(message_index, part_index)) else {
                    content.push(part);
                    continue;
                };
                if let Some(projected) = projected_by_asset.get(asset_id) {
                    content.push((*projected).clone());
                } else {
                    content.push(ContentPart::text(format!(
                        "[附件: {}；当前模型不支持安全的媒体输入，已降级为文本占位]",
                        if matches!(part, ContentPart::Image { .. }) {
                            "图片"
                        } else {
                            "音频"
                        }
                    )));
                }
            }
            message.content = content;
        }
        let repairs = crate::sanitize_canonical(&mut messages);
        (
            Self {
                messages,
                media_inputs: self.media_inputs.clone(),
                repairs,
            },
            plan,
        )
    }

    pub(super) fn messages(&self) -> &[CanonicalMessage] {
        &self.messages
    }

    pub(super) fn media_requirements(&self) -> MediaRequirements {
        canonical_media_requirements(&self.messages)
    }

    /// Check whether every raw image/audio part can remain raw for a role.
    /// This is used before choosing a dedicated modality role, so an
    /// unsupported MIME, size limit, part limit, or malformed raw projection
    /// can trigger a retry through the default role instead of silently
    /// becoming a placeholder on the specialized endpoint.
    pub(super) fn raw_media_fits_profile(&self, capabilities: &CapabilityProfile) -> bool {
        let inputs: Vec<_> = self
            .messages
            .iter()
            .enumerate()
            .flat_map(|(message_index, message)| {
                message
                    .content
                    .iter()
                    .enumerate()
                    .filter_map(move |(part_index, part)| {
                        let fallback = attachment_from_content_part(part)?;
                        Some(
                            self.media_inputs
                                .get(message_index)
                                .and_then(|parts| parts.get(part_index))
                                .and_then(Option::as_ref)
                                .cloned()
                                .unwrap_or_else(|| legacy_attachment_to_media_input(&fallback)),
                        )
                    })
            })
            .collect();
        if inputs.is_empty() {
            return true;
        }
        let plan = build_media_plan(&inputs, capabilities, MediaInputStrategy::RawPreferred);
        if plan.projections.len() != inputs.len()
            || plan
                .projections
                .iter()
                .any(|projection| projection.mode != MediaProjectionMode::Raw)
        {
            return false;
        }
        haven_llm::media::project_media_plan(&plan, &inputs)
            .is_ok_and(|parts| parts.len() == inputs.len())
    }

    pub(super) fn repairs(&self) -> usize {
        self.repairs
    }

    fn from_messages(
        mut messages: Vec<CanonicalMessage>,
        mut media_inputs: Vec<Vec<Option<MediaInput>>>,
    ) -> Self {
        let repairs = crate::sanitize_canonical(&mut messages);
        media_inputs.resize_with(messages.len(), Vec::new);
        for (parts, message) in media_inputs.iter_mut().zip(&messages) {
            parts.resize(message.content.len(), None);
        }
        Self {
            messages,
            media_inputs,
            repairs,
        }
    }
}

/// Reconnect durable `MediaInput` metadata with the raw content parts in the
/// provider request. `ContentPart` intentionally has no asset id, therefore
/// this association must be recovered from the event-backed transcript rather
/// than by hashing or minting a replacement id.
fn media_inputs_for_state(
    state: &ReActState,
    messages: &[CanonicalMessage],
) -> Vec<Vec<Option<MediaInput>>> {
    let mut compact_inputs: Option<Vec<MediaInput>> = None;
    let mut compacted_messages: Option<Vec<CanonicalMessage>> = None;
    let mut event_inputs: Vec<Vec<MediaInput>> = Vec::new();
    for event in &state.events {
        match event {
            TranscriptRecord::CompactSummary {
                compacted,
                media_inputs,
                ..
            } => {
                compact_inputs = Some(media_inputs.clone());
                compacted_messages = Some(compacted.clone());
                event_inputs.clear();
            }
            TranscriptRecord::UserInject {
                media_inputs,
                attachments,
                ..
            } => {
                let inputs = if media_inputs.is_empty() {
                    attachments
                        .iter()
                        .map(legacy_attachment_to_media_input)
                        .collect()
                } else {
                    media_inputs.clone()
                };
                // Keep empty groups: source-tagged canonical user messages
                // consume one UserInject per message, even when that inject
                // has no attachments. Dropping the empty group would shift
                // every later asset group by one.
                event_inputs.push(inputs);
            }
            _ => {}
        }
    }

    // The live state keeps raw parts for the current request, while the
    // CompactSummary event keeps the same messages with raw parts replaced by
    // identity-bearing markers. Use those markers to reconnect retained raw
    // parts to their original assets, including source-tagged messages that
    // no longer have a corresponding UserInject after compaction.
    let compact_message_inputs = match (compacted_messages.as_deref(), compact_inputs.as_deref()) {
        (Some(compacted), Some(inputs)) => compacted
            .iter()
            .map(|message| snapshot_media_inputs_for_message(message, inputs))
            .collect::<Vec<_>>(),
        _ => Vec::new(),
    };

    let mut next_event = 0;
    messages
        .iter()
        .enumerate()
        .map(|(message_index, message)| {
            let is_user = message.role == haven_common::types::CanonicalRole::User;
            let durable = if let Some(compact) = compact_message_inputs.get(message_index) {
                Some(compact.as_slice())
            } else if !is_user {
                None
            } else if message.source.is_some() {
                // A user injection consumes its event group even when all of
                // its attachments were projected to derived text. This is
                // the part the old raw-count cursor got wrong.
                let result = event_inputs.get(next_event);
                next_event += 1;
                result.map(Vec::as_slice)
            } else if compact_inputs.is_some() {
                // Legacy compact roots may not have identity-bearing
                // per-message markers. Keep a conservative modality-only
                // fallback for those roots.
                compact_inputs.as_deref()
            } else {
                let result = event_inputs.get(next_event);
                next_event += 1;
                result.map(Vec::as_slice)
            };
            let mut local_used = std::collections::HashSet::new();
            message
                .content
                .iter()
                .map(|part| {
                    let modality = match part {
                        ContentPart::Image { .. } => Some(MediaModality::Image),
                        ContentPart::Audio { .. } => Some(MediaModality::Audio),
                        ContentPart::Text(_) => None,
                    }?;
                    let inputs = durable?;
                    let index = inputs.iter().enumerate().find_map(|(index, input)| {
                        if local_used.contains(&index) || !media_input_matches_part(input, modality)
                        {
                            return None;
                        }
                        local_used.insert(index);
                        Some(index)
                    })?;
                    inputs
                        .get(index)
                        .cloned()
                        .map(|input| restore_raw_part(input, part))
                })
                .collect()
        })
        .collect()
}

fn snapshot_media_inputs_for_message(
    message: &CanonicalMessage,
    inputs: &[MediaInput],
) -> Vec<MediaInput> {
    let mut used = std::collections::HashSet::new();
    message
        .content
        .iter()
        .filter_map(|part| {
            let ContentPart::Text(text) = part else {
                return None;
            };
            let (modality, asset_id) = snapshot_media_marker_info(text)?;
            let index = asset_id
                .as_deref()
                .and_then(|asset_id| {
                    inputs.iter().enumerate().find_map(|(index, input)| {
                        (input.asset.asset_id == asset_id && used.insert(index)).then_some(index)
                    })
                })
                .or_else(|| {
                    inputs.iter().enumerate().find_map(|(index, input)| {
                        if used.contains(&index) || !media_input_matches_part(input, modality) {
                            return None;
                        }
                        used.insert(index);
                        Some(index)
                    })
                })?;
            inputs.get(index).cloned()
        })
        .collect()
}

fn snapshot_media_marker_info(text: &str) -> Option<(MediaModality, Option<String>)> {
    let modality = if text.starts_with("[managed image omitted from snapshot;") {
        MediaModality::Image
    } else if text.starts_with("[managed audio omitted from snapshot;") {
        MediaModality::Audio
    } else {
        return None;
    };
    let asset_id = text
        .split_once("asset_id=")
        .and_then(|(_, value)| value.split(';').next())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned);
    Some((modality, asset_id))
}

fn restore_raw_part(mut input: MediaInput, part: &ContentPart) -> MediaInput {
    let (kind, media_type, data) = match part {
        ContentPart::Image {
            media_type, data, ..
        } => (
            haven_common::media::MediaRepresentationKind::RawImage,
            media_type,
            data,
        ),
        ContentPart::Audio {
            media_type, data, ..
        } => (
            haven_common::media::MediaRepresentationKind::RawAudio,
            media_type,
            data,
        ),
        ContentPart::Text(_) => return input,
    };
    let payload = haven_common::media::MediaRepresentationPayload::InlineData {
        media_type: media_type.clone(),
        data: data.clone(),
    };
    if let Some(representation) = input
        .representations
        .iter_mut()
        .find(|representation| representation.representation == kind)
    {
        representation.payload = payload;
        representation.availability =
            haven_common::media::MediaRepresentationAvailability::Available;
    } else {
        input
            .representations
            .push(haven_common::media::MediaRepresentation::available(
                kind,
                haven_common::media::MediaProvenance::Original,
                payload,
            ));
    }
    // The current canonical part is authoritative for this request. A
    // snapshot may have converted a raw preference into managed_file_ref only
    // to avoid persisting bytes; restore the raw preference in the ephemeral
    // request input while retaining the durable asset identity.
    input.preferred_representation = None;
    input
}

fn media_input_matches_part(input: &MediaInput, modality: MediaModality) -> bool {
    input.asset.media_type.starts_with(match modality {
        MediaModality::Image => "image/",
        MediaModality::Audio => "audio/",
        _ => return false,
    }) || input
        .representations
        .iter()
        .any(|representation| representation.representation.raw_modality() == Some(modality))
}

fn attachment_from_content_part(part: &ContentPart) -> Option<MessageAttachment> {
    let (media_type, data) = match part {
        ContentPart::Image {
            media_type, data, ..
        }
        | ContentPart::Audio {
            media_type, data, ..
        } => (media_type, data),
        ContentPart::Text(_) => return None,
    };
    let mut attachment = MessageAttachment::new(media_type.clone(), data.clone());
    // Keep this metadata-only adapter explicit. Paths and managed ids are not
    // recoverable from provider-neutral raw parts and must never be guessed.
    attachment.asset_id = None;
    Some(attachment)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::BranchPoint;
    use haven_common::types::{CanonicalToolCall, ContentPart};
    use std::collections::HashMap;

    fn state(messages: Vec<CanonicalMessage>) -> ReActState {
        ReActState::new(Vec::new(), messages, HashMap::<u32, BranchPoint>::new())
    }

    #[test]
    fn request_context_does_not_mutate_durable_canonical() {
        let durable = vec![CanonicalMessage::user_text("hello")];
        let context = RequestContext::from_state(&state(durable.clone()), None);

        assert_eq!(context.messages().len(), durable.len());
        assert_eq!(
            context.messages()[0].content.len(),
            durable[0].content.len()
        );
    }

    #[test]
    fn retry_nudge_is_applied_to_the_request_copy_only() {
        let durable = vec![
            CanonicalMessage::assistant(
                Vec::new(),
                Some(vec![CanonicalToolCall {
                    id: "call-1".into(),
                    name: "read_file".into(),
                    arguments: serde_json::json!({"path": "notes.txt"}),
                }]),
                None,
                Vec::new(),
                Vec::new(),
            ),
            CanonicalMessage::tool(
                vec![ContentPart::text("failed")],
                Some("call-1".to_string()),
            ),
        ];
        let mut run = state(durable.clone());
        run.stage_retry_nudge("call-1".into(), "retry safely".into());
        let nudge = run.take_retry_nudge();
        let context = RequestContext::from_state(&run, nudge.as_ref());

        assert_eq!(run.canonical.len(), durable.len());
        assert!(matches!(
            run.canonical[1].content.as_slice(),
            [ContentPart::Text(text)] if text == "failed"
        ));
        assert!(context.messages()[1].content.iter().any(|part| {
            matches!(part, ContentPart::Text(text) if text.contains("retry safely"))
        }));
    }

    #[test]
    fn user_instruction_creates_a_fresh_snapshot_without_accumulation() {
        let base =
            RequestContext::from_state(&state(vec![CanonicalMessage::user_text("hello")]), None);
        let first = base.with_user_instruction("first");
        let second = first.with_user_instruction("second");

        assert_eq!(base.messages().len(), 1);
        assert_eq!(first.messages().len(), 2);
        assert_eq!(second.messages().len(), 3);
        assert!(
            first.messages()[1]
                .content
                .iter()
                .any(|part| { matches!(part, ContentPart::Text(text) if text == "first") })
        );
    }

    #[test]
    fn media_is_replanned_against_the_selected_adapter_profile() {
        let image = CanonicalMessage::user(vec![ContentPart::Image {
            content_type: "image".into(),
            media_type: "image/png".into(),
            data: "aGVsbG8=".into(),
        }]);
        let context = RequestContext::from_state(&state(vec![image]), None);
        let profile = CapabilityProfile {
            image: haven_common::media::CapabilitySupport::Unsupported,
            ..CapabilityProfile::default()
        };

        let (planned, plan) = context.with_capabilities(&profile, MediaInputStrategy::Auto);

        assert!(
            !planned.messages()[0]
                .content
                .iter()
                .any(|part| matches!(part, ContentPart::Image { .. }))
        );
        assert!(planned.messages()[0].content.iter().any(|part| {
            matches!(part, ContentPart::Text(text) if text.contains("降级为文本占位"))
        }));
        assert!(plan.notices.iter().any(|notice| {
            notice.code == haven_common::media::MediaPlanNoticeCode::RawCapabilityUnsupported
        }));
    }

    #[test]
    fn media_replanning_applies_request_level_part_limits() {
        let image = |data: &str| ContentPart::Image {
            content_type: "image".into(),
            media_type: "image/png".into(),
            data: data.into(),
        };
        let context = RequestContext::from_state(
            &state(vec![CanonicalMessage::user(vec![
                image("aGVsbG8="),
                image("d29ybGQ="),
            ])]),
            None,
        );
        let profile = CapabilityProfile {
            image: haven_common::media::CapabilitySupport::Supported,
            max_input_parts: Some(1),
            ..CapabilityProfile::default()
        };

        let (planned, plan) = context.with_capabilities(&profile, MediaInputStrategy::Auto);
        let content = &planned.messages()[0].content;
        assert_eq!(
            content
                .iter()
                .filter(|part| matches!(part, ContentPart::Image { .. }))
                .count(),
            1
        );
        assert_eq!(
            content
                .iter()
                .filter(|part| matches!(part, ContentPart::Text(text) if text.contains("降级为文本占位")))
                .count(),
            1
        );
        assert!(plan.notices.iter().any(|notice| {
            notice.code == haven_common::media::MediaPlanNoticeCode::InputPartLimit
        }));
    }

    #[test]
    fn media_plan_reuses_durable_asset_id_instead_of_minting_a_synthetic_id() {
        let asset_id = "asset-0123456789abcdef0123456789abcdef";
        let mut attachment = MessageAttachment::new("image/png", "aGVsbG8=");
        attachment.asset_id = Some(asset_id.into());
        attachment.path = Some(r"C:\Users\olive\uploads\photo.png".into());
        let input = legacy_attachment_to_media_input(&attachment);
        let event = TranscriptRecord::UserInject {
            step_number: 1,
            source: haven_common::types::InjectSource::FollowUp,
            text: "看图".into(),
            media_inputs: vec![input],
            attachments: Vec::new(),
            message_id: None,
        };
        let message = CanonicalMessage::user(vec![
            ContentPart::text("看图"),
            ContentPart::Image {
                content_type: "image".into(),
                media_type: "image/png".into(),
                data: "aGVsbG8=".into(),
            },
        ]);
        let context = RequestContext::from_state(
            &ReActState::new(vec![event], vec![message], HashMap::new()),
            None,
        );
        let profile = CapabilityProfile {
            image: haven_common::media::CapabilitySupport::Supported,
            ..CapabilityProfile::default()
        };

        let (_, plan) = context.with_capabilities(&profile, MediaInputStrategy::Auto);

        assert_eq!(plan.projections[0].asset_id, asset_id);
    }

    #[test]
    fn mixed_derived_then_raw_media_keeps_the_later_audio_identity() {
        let image_id = "asset-11111111111111111111111111111111";
        let audio_id = "asset-22222222222222222222222222222222";
        let mut image = MessageAttachment::new("image/png", "aW1hZ2U=");
        image.asset_id = Some(image_id.into());
        image.path = Some(r"C:\haven\uploads\image.png".into());
        image.preferred_representation =
            Some(haven_common::media::MediaRepresentationKind::OcrText);
        image
            .representations
            .push(haven_common::media::MediaRepresentation::available(
                haven_common::media::MediaRepresentationKind::OcrText,
                haven_common::media::MediaProvenance::Derived {
                    operation: haven_common::media::MediaDerivation::Ocr,
                    provider: None,
                    source_kind: Some(haven_common::media::MediaRepresentationKind::RawImage),
                },
                haven_common::media::MediaRepresentationPayload::Text("文字".into()),
            ));
        let mut audio = MessageAttachment::new("audio/wav", "YXVkaW8=");
        audio.asset_id = Some(audio_id.into());
        audio.path = Some(r"C:\haven\uploads\audio.wav".into());

        let events = vec![
            TranscriptRecord::UserInject {
                step_number: 1,
                source: haven_common::types::InjectSource::FollowUp,
                text: "先读图".into(),
                media_inputs: vec![legacy_attachment_to_media_input(&image)],
                attachments: Vec::new(),
                message_id: Some("msg-11111111111111111111111111111111".into()),
            },
            TranscriptRecord::UserInject {
                step_number: 2,
                source: haven_common::types::InjectSource::FollowUp,
                text: "再听音频".into(),
                media_inputs: vec![legacy_attachment_to_media_input(&audio)],
                attachments: Vec::new(),
                message_id: Some("msg-22222222222222222222222222222222".into()),
            },
        ];
        let (messages, _) =
            crate::types::project_transcript_with_strategy(&events, MediaInputStrategy::Auto);
        assert!(matches!(messages[0].content[1], ContentPart::Text(_)));
        assert!(matches!(messages[1].content[1], ContentPart::Audio { .. }));

        let context =
            RequestContext::from_state(&ReActState::new(events, messages, HashMap::new()), None);
        let (_, plan) = context.with_capabilities(
            &CapabilityProfile {
                audio: haven_common::media::CapabilitySupport::Supported,
                ..CapabilityProfile::default()
            },
            MediaInputStrategy::Auto,
        );

        assert_eq!(plan.projections.len(), 1);
        assert_eq!(plan.projections[0].asset_id, audio_id);
    }

    #[test]
    fn compacted_source_tagged_media_reconnects_through_snapshot_marker() {
        let asset_id = "asset-33333333333333333333333333333333";
        let mut attachment = MessageAttachment::new("audio/wav", "YXVkaW8=");
        attachment.asset_id = Some(asset_id.into());
        attachment.path = Some(r"C:\haven\uploads\recording.wav".into());
        let input = legacy_attachment_to_media_input(&attachment);
        let live_message = CanonicalMessage::user_with_source(
            vec![
                ContentPart::text("继续处理录音"),
                ContentPart::Audio {
                    content_type: "audio".into(),
                    media_type: "audio/wav".into(),
                    data: "YXVkaW8=".into(),
                },
            ],
            haven_common::types::InjectSource::FollowUp,
        );
        let snapshot_message = crate::types::canonical_for_snapshot_with_media_inputs(
            std::slice::from_ref(&live_message),
            std::slice::from_ref(&input),
        );
        let state = ReActState::new(
            vec![TranscriptRecord::CompactSummary {
                compacted: snapshot_message.clone(),
                media_inputs: vec![input.for_snapshot()],
                summary: "older context".into(),
                tokens_before: 100,
                tokens_after: 20,
                episode_id: "msg-33333333333333333333333333333333".into(),
                degraded: false,
            }],
            vec![live_message],
            HashMap::new(),
        );
        let context = RequestContext::from_state(&state, None);
        let (_, plan) = context.with_capabilities(
            &CapabilityProfile {
                audio: haven_common::media::CapabilitySupport::Supported,
                ..CapabilityProfile::default()
            },
            MediaInputStrategy::Auto,
        );

        assert_eq!(plan.projections.len(), 1);
        assert_eq!(plan.projections[0].asset_id, asset_id);
    }
}
