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
    CapabilityProfile, MediaInput, MediaInputStrategy, MediaPlan, MediaProjectionMode,
    build_media_plan, legacy_attachment_to_media_input,
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
    let mut event_inputs: Vec<Vec<MediaInput>> = Vec::new();
    for event in &state.events {
        match event {
            TranscriptRecord::CompactSummary { .. } => event_inputs.clear(),
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
                if !inputs.is_empty() {
                    event_inputs.push(inputs);
                }
            }
            _ => {}
        }
    }

    let mut next_event = 0;
    messages
        .iter()
        .map(|message| {
            let media_part_count = message
                .content
                .iter()
                .filter(|part| {
                    matches!(part, ContentPart::Image { .. } | ContentPart::Audio { .. })
                })
                .count();
            let durable = if media_part_count == 0 {
                None
            } else {
                let result = event_inputs.get(next_event);
                next_event += 1;
                result
            };
            let mut next_input = 0;
            message
                .content
                .iter()
                .map(|part| {
                    if matches!(part, ContentPart::Image { .. } | ContentPart::Audio { .. }) {
                        let input = durable.and_then(|inputs| inputs.get(next_input)).cloned();
                        next_input += 1;
                        input
                    } else {
                        None
                    }
                })
                .collect()
        })
        .collect()
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
}
