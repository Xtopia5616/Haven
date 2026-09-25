//! Logical request purpose and the model capability required to serve it.
//!
//! `RequestKind` remains the configured route key. This crate-private
//! descriptor carries the request's capability requirement into execution
//! without asking each executor to derive it again.

use haven_common::config::{Capability, RequestKind};

/// Request semantics shared by route resolution and execution boundaries.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct RequestDescriptor {
    pub(crate) purpose: RequestKind,
    pub(crate) required_capability: Capability,
}

impl From<RequestKind> for RequestDescriptor {
    fn from(purpose: RequestKind) -> Self {
        Self {
            purpose,
            required_capability: purpose.required_capability(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_request_kind_keeps_its_explicit_capability_mapping() {
        let expected = [
            (RequestKind::Chat, Capability::Chat),
            (RequestKind::FastChat, Capability::FastChat),
            (RequestKind::Vision, Capability::Vision),
            (RequestKind::AudioChat, Capability::AudioInput),
            (RequestKind::Transcription, Capability::Transcription),
            (RequestKind::Embedding, Capability::Embedding),
            (RequestKind::ImageGeneration, Capability::ImageGeneration),
            (RequestKind::SpeechSynthesis, Capability::SpeechSynthesis),
        ];
        assert_eq!(RequestKind::ALL.len(), expected.len());

        for (purpose, capability) in expected {
            assert!(RequestKind::ALL.contains(&purpose));
            let descriptor = RequestDescriptor::from(purpose);
            assert_eq!(descriptor.purpose, purpose);
            assert_eq!(descriptor.required_capability, capability);
            assert_eq!(
                descriptor.required_capability,
                purpose.required_capability()
            );
        }
    }

    #[test]
    fn similar_request_purposes_keep_distinct_capabilities() {
        let chat = RequestDescriptor::from(RequestKind::Chat);
        let fast_chat = RequestDescriptor::from(RequestKind::FastChat);
        let audio_chat = RequestDescriptor::from(RequestKind::AudioChat);
        let transcription = RequestDescriptor::from(RequestKind::Transcription);

        assert_eq!(chat.required_capability, Capability::Chat);
        assert_eq!(fast_chat.required_capability, Capability::FastChat);
        assert_eq!(audio_chat.required_capability, Capability::AudioInput);
        assert_eq!(transcription.required_capability, Capability::Transcription);
        assert_ne!(audio_chat.purpose, transcription.purpose);
    }
}
