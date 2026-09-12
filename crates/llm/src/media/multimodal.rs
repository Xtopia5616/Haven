//! Multimodal input encoding for already-planned media representations.
//!
//! Images become vision content parts, audio becomes STT content parts, and
//! video remains a native provider-neutral payload until adapter projection.
//! Raw-byte callers use the higher-level media entry points, which perform
//! capability planning before reaching these pre-encoded constructors.

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

/// Build an audio content part from an already-encoded base64 payload.
/// Keeping this alongside [`image_part`] makes attachment injection and
/// gateway fallbacks use the same provider-neutral shape.
pub fn audio_part(media_type: &str, base64_data: String) -> ContentPart {
    ContentPart::Audio {
        content_type: "input_audio".into(),
        media_type: media_type.to_string(),
        data: base64_data,
    }
}

/// Build a video content part from already-encoded base64 payload. The
/// selected adapter is responsible for supporting its native video wire.
pub fn video_part(media_type: &str, base64_data: String) -> ContentPart {
    ContentPart::Video {
        content_type: "input_video".into(),
        media_type: media_type.to_string(),
        data: base64_data,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use haven_common::types::ContentPart;

    #[test]
    fn test_preencoded_parts_passthrough() {
        let part = image_part("image/jpeg", "QUJD".into());
        match part {
            ContentPart::Image { data, .. } => assert_eq!(data, "QUJD"),
            _ => panic!("expected Image part"),
        }
    }

    #[test]
    fn test_preencoded_audio_part_passthrough() {
        let part = audio_part("audio/mpeg", "SUQz".into());
        match part {
            ContentPart::Audio {
                media_type, data, ..
            } => {
                assert_eq!(media_type, "audio/mpeg");
                assert_eq!(data, "SUQz");
            }
            _ => panic!("expected Audio part"),
        }
    }

    #[test]
    fn test_preencoded_video_part_passthrough() {
        let part = video_part("video/mp4", "TVA0".into());
        match part {
            ContentPart::Video {
                media_type, data, ..
            } => {
                assert_eq!(media_type, "video/mp4");
                assert_eq!(data, "TVA0");
            }
            _ => panic!("expected Video part"),
        }
    }
}
