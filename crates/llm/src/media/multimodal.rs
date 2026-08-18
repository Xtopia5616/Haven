//! Multimodal input encoding: raw media bytes → LLM content parts.
//!
//! Images become vision content parts (base64 payload) routed through the
//! router's vision role; audio becomes STT content parts. Both the media
//! gateway's main-model fallback and the agent's attachment injection build
//! their parts here so the wire format stays unified.

use base64::Engine;
use haven_common::types::ContentPart;

/// Build an image content part from already-encoded base64 payload
/// (e.g. a stored attachment).
pub fn image_part(media_type: &str, base64_data: String) -> ContentPart {
    ContentPart::Image {
        content_type: "image_url".into(),
        media_type: media_type.to_string(),
        data: base64_data,
    }
}

/// Encode raw image bytes (base64) and build the image content part.
pub fn image_part_from_bytes(media_type: &str, bytes: &[u8]) -> ContentPart {
    image_part(
        media_type,
        base64::engine::general_purpose::STANDARD.encode(bytes),
    )
}

/// Encode raw audio bytes (base64) and build the audio content part.
pub fn audio_part_from_bytes(media_type: &str, bytes: &[u8]) -> ContentPart {
    ContentPart::Audio {
        content_type: "input_audio".into(),
        media_type: media_type.to_string(),
        data: base64::engine::general_purpose::STANDARD.encode(bytes),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use haven_common::types::ContentPart;

    #[test]
    fn test_image_part_shape() {
        let part = image_part_from_bytes("image/png", b"\x89PNG\x0d\x0a");
        match part {
            ContentPart::Image {
                content_type,
                media_type,
                data,
            } => {
                assert_eq!(content_type, "image_url");
                assert_eq!(media_type, "image/png");
                assert!(data.starts_with("iVBORw0K"));
            }
            _ => panic!("expected Image part"),
        }
    }

    #[test]
    fn test_audio_part_shape() {
        let part = audio_part_from_bytes("audio/wav", b"RIFF....WAVE");
        match part {
            ContentPart::Audio {
                content_type,
                media_type,
                data,
            } => {
                assert_eq!(content_type, "input_audio");
                assert_eq!(media_type, "audio/wav");
                assert!(!data.is_empty());
            }
            _ => panic!("expected Audio part"),
        }
    }

    #[test]
    fn test_preencoded_parts_passthrough() {
        let part = image_part("image/jpeg", "QUJD".into());
        match part {
            ContentPart::Image { data, .. } => assert_eq!(data, "QUJD"),
            _ => panic!("expected Image part"),
        }
    }
}
