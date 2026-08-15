//! Stage 3: routing decisions — the (modality × intent) coverage table.
//!
//! Only high-cost extraction tasks get dedicated providers (OCR / ASR);
//! everything else passes through to the main model. The table is a pure
//! function of `(Modality, Intent)` so it can be unit-tested and rendered
//! for debugging without touching the network.

use crate::intent::Intent;
use crate::modality::Modality;

/// What should happen to an input after detection and classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CoverageAction {
    /// Image → dedicated OCR provider (media.ocr).
    Ocr,
    /// Audio → dedicated ASR provider (media.stt).
    Stt,
    /// Pass through as image content parts → image_model / default model.
    LlmImage,
    /// Pass through as audio content parts → audio_model / default model.
    LlmAudio,
    /// Pass through as plain text/reference → default model.
    LlmDefault,
    /// Text → TTS (media.tts).
    Tts,
    /// Text → text-to-image (media.image_gen).
    ImageGen,
}

impl CoverageAction {
    pub const fn as_str(self) -> &'static str {
        match self {
            CoverageAction::Ocr => "ocr",
            CoverageAction::Stt => "stt",
            CoverageAction::LlmImage => "llm:image",
            CoverageAction::LlmAudio => "llm:audio",
            CoverageAction::LlmDefault => "llm:default",
            CoverageAction::Tts => "tts",
            CoverageAction::ImageGen => "image_gen",
        }
    }
}

/// The routing decision for one input: what was detected, what was
/// classified, where it goes, and whether a fallback happened.
#[derive(Debug, Clone)]
pub struct MediaDecision {
    pub modality: Modality,
    pub intent: Intent,
    pub action: CoverageAction,
    /// Canonical routing target ([`CoverageAction::as_str`]).
    pub routed_to: String,
    /// True when a specialized provider was attempted first but the result
    /// fell below the confidence threshold (or failed), so the main model
    /// handled it instead.
    pub fallback: bool,
}

impl MediaDecision {
    pub fn new(modality: Modality, intent: Intent, action: CoverageAction) -> Self {
        Self {
            modality,
            intent,
            action,
            routed_to: action.as_str().to_string(),
            fallback: false,
        }
    }
}

/// Resolve the coverage action for an input attachment.
///
/// Generate intent on an attachment collapses to extraction/pass-through:
/// media *from* files is not a v1 generation target, except the speech
/// keywords with an audio/video file ("把这段音频读出来" = transcribe it).
pub fn coverage_for(modality: Modality, intent: Intent) -> CoverageAction {
    match (modality, intent) {
        (Modality::Image, Intent::Extract) => CoverageAction::Ocr,
        (Modality::Audio, Intent::Extract) => CoverageAction::Stt,
        (Modality::Audio, Intent::Generate) => CoverageAction::Stt,
        (Modality::Image, Intent::Understand) => CoverageAction::LlmImage,
        (Modality::Image, Intent::Generate) => CoverageAction::LlmImage,
        (Modality::Audio, Intent::Understand) => CoverageAction::LlmAudio,
        // Video extraction needs an audio-track extractor (ffmpeg), which is
        // out of v1 scope — the main model handles video (Gemini accepts
        // video inline; other providers surface an unsupported error).
        (Modality::Video, Intent::Extract) => CoverageAction::LlmDefault,
        (Modality::Video, Intent::Generate) => CoverageAction::LlmDefault,
        (Modality::Video, Intent::Understand) => CoverageAction::LlmDefault,
        (Modality::Text, _) | (Modality::Document, _) | (Modality::Unknown, _) => {
            CoverageAction::LlmDefault
        }
    }
}

/// Resolve the coverage action for a pure-text generate request (no
/// attachment). Image generation is the ambiguous default; speech keywords
/// (朗读/读出来/配音…) route to TTS.
pub fn coverage_for_generate(user_text: &str) -> CoverageAction {
    match crate::intent::detect_generate_kind(user_text) {
        crate::intent::GenerateKind::Speech => CoverageAction::Tts,
        crate::intent::GenerateKind::Image => CoverageAction::ImageGen,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_routes_to_dedicated_providers() {
        assert_eq!(
            coverage_for(Modality::Image, Intent::Extract),
            CoverageAction::Ocr
        );
        assert_eq!(
            coverage_for(Modality::Audio, Intent::Extract),
            CoverageAction::Stt
        );
        // "把这段音频读出来" → generate keywords, but the input is audio.
        assert_eq!(
            coverage_for(Modality::Audio, Intent::Generate),
            CoverageAction::Stt
        );
    }

    #[test]
    fn understand_routes_by_modality() {
        assert_eq!(
            coverage_for(Modality::Image, Intent::Understand),
            CoverageAction::LlmImage
        );
        assert_eq!(
            coverage_for(Modality::Audio, Intent::Understand),
            CoverageAction::LlmAudio
        );
        assert_eq!(
            coverage_for(Modality::Text, Intent::Understand),
            CoverageAction::LlmDefault
        );
        assert_eq!(
            coverage_for(Modality::Document, Intent::Understand),
            CoverageAction::LlmDefault
        );
    }

    #[test]
    fn video_and_unknown_pass_through() {
        for intent in [Intent::Extract, Intent::Understand, Intent::Generate] {
            assert_eq!(
                coverage_for(Modality::Video, intent),
                CoverageAction::LlmDefault
            );
            assert_eq!(
                coverage_for(Modality::Unknown, intent),
                CoverageAction::LlmDefault
            );
        }
    }

    #[test]
    fn generate_kind_routes() {
        assert_eq!(coverage_for_generate("朗读这段文字"), CoverageAction::Tts);
        assert_eq!(coverage_for_generate("画一只猫"), CoverageAction::ImageGen);
    }

    #[test]
    fn action_strings_are_stable() {
        assert_eq!(CoverageAction::Ocr.as_str(), "ocr");
        assert_eq!(CoverageAction::Stt.as_str(), "stt");
        assert_eq!(CoverageAction::LlmImage.as_str(), "llm:image");
        assert_eq!(CoverageAction::LlmAudio.as_str(), "llm:audio");
        assert_eq!(CoverageAction::LlmDefault.as_str(), "llm:default");
        assert_eq!(CoverageAction::Tts.as_str(), "tts");
        assert_eq!(CoverageAction::ImageGen.as_str(), "image_gen");
    }
}
