//! Immutable provider-request context for one ReAct turn.
//!
//! The run state is durable transcript state. A provider request is an
//! ephemeral projection of that state: it may contain a retry hint, a
//! cut-off instruction, or repairs required by a provider's tool-call
//! contract, but none of those changes belong in the transcript. Keeping this
//! projection in one type makes it impossible for Turn, retry, and compaction
//! paths to each invent their own clone/append/sanitize sequence.

use super::{MediaRequirements, ReActEngine, ReActState, RetryNudge, canonical_media_requirements};
use haven_common::media::{CapabilityProfile, MediaInputStrategy, MediaPlanNotice};
use haven_common::types::{CanonicalMessage, ContentPart, MessageAttachment};

/// One immutable provider request snapshot.
#[derive(Debug, Clone)]
pub(crate) struct RequestContext {
    messages: Vec<CanonicalMessage>,
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
        Self::from_messages(messages)
    }

    /// Build a new request view with a trailing, provider-only user
    /// instruction. Used by the cut-off retry. The original snapshot remains
    /// untouched, so retries cannot accidentally accumulate instructions.
    pub(super) fn with_user_instruction(&self, instruction: impl Into<String>) -> Self {
        let mut messages = self.messages.clone();
        messages.push(CanonicalMessage::user_text(instruction));
        Self::from_messages(messages)
    }

    /// Re-project raw media against the selected adapter's actual wire
    /// capabilities. Durable canonical state stays provider-neutral; this
    /// request-only copy may replace an unsafe raw part with an explicit safe
    /// text fallback and returns stable diagnostics for the UI/log.
    pub(super) fn with_capabilities(
        &self,
        capabilities: &CapabilityProfile,
        strategy: MediaInputStrategy,
    ) -> (Self, Vec<MediaPlanNotice>) {
        let mut messages = self.messages.clone();
        let mut notices = Vec::new();
        for message in &mut messages {
            let mut content = Vec::with_capacity(message.content.len());
            for part in &message.content {
                let Some(attachment) = attachment_from_content_part(part) else {
                    content.push(part.clone());
                    continue;
                };
                let input = haven_common::media::legacy_attachment_to_media_input(&attachment);
                let plan = haven_common::media::build_media_plan(
                    std::slice::from_ref(&input),
                    capabilities,
                    strategy,
                );
                notices.extend(plan.notices.clone());
                match haven_llm::media::project_media_plan(&plan, std::slice::from_ref(&input)) {
                    Ok(mut projected) if !projected.is_empty() => content.append(&mut projected),
                    _ => content.push(ContentPart::text(format!(
                        "[附件: {}；当前模型不支持安全的媒体输入，已降级为文本占位]",
                        if attachment.is_image() {
                            "图片"
                        } else {
                            "音频"
                        }
                    ))),
                }
            }
            message.content = content;
        }
        let repairs = crate::sanitize_canonical(&mut messages);
        (Self { messages, repairs }, notices)
    }

    pub(super) fn messages(&self) -> &[CanonicalMessage] {
        &self.messages
    }

    pub(super) fn media_requirements(&self) -> MediaRequirements {
        canonical_media_requirements(&self.messages)
    }

    pub(super) fn repairs(&self) -> usize {
        self.repairs
    }

    fn from_messages(mut messages: Vec<CanonicalMessage>) -> Self {
        let repairs = crate::sanitize_canonical(&mut messages);
        Self { messages, repairs }
    }
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

        let (planned, notices) = context.with_capabilities(&profile, MediaInputStrategy::Auto);

        assert!(
            !planned.messages()[0]
                .content
                .iter()
                .any(|part| matches!(part, ContentPart::Image { .. }))
        );
        assert!(planned.messages()[0].content.iter().any(|part| {
            matches!(part, ContentPart::Text(text) if text.contains("降级为文本占位"))
        }));
        assert!(notices.iter().any(|notice| {
            notice.code == haven_common::media::MediaPlanNoticeCode::RawCapabilityUnsupported
        }));
    }
}
