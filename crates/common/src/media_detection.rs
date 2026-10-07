//! Canonical media probing shared by ingress, files, tools and providers.
//!
//! Detection is deliberately deterministic and dependency-free from business
//! orchestration: recognizable bytes win, then the filename extension, then
//! text decoding, and finally `Unknown`. A caller-provided MIME value is only
//! used as a last-resort hint by [`probe_media_with_hint`].

use serde::{Deserialize, Serialize};

/// Coarse media kind detected at filesystem and ingress boundaries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DetectedMediaKind {
    Text,
    Image,
    Audio,
    Video,
    Document,
    Unknown,
}

impl DetectedMediaKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Image => "image",
            Self::Audio => "audio",
            Self::Video => "video",
            Self::Document => "document",
            Self::Unknown => "unknown",
        }
    }

    pub const fn is_rich_media(self) -> bool {
        matches!(
            self,
            Self::Image | Self::Audio | Self::Video | Self::Document
        )
    }
}

/// Result of the canonical content/filename probe.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MediaProbe {
    pub media_kind: DetectedMediaKind,
    pub mime_type: String,
}

impl MediaProbe {
    pub fn new(media_kind: DetectedMediaKind, mime_type: impl Into<String>) -> Self {
        Self {
            media_kind,
            mime_type: mime_type.into(),
        }
    }
}

/// Detect the coarse type of raw bytes, using `filename` only when magic bytes
/// are inconclusive.
pub fn detect_media_kind(data: &[u8], filename: &str) -> DetectedMediaKind {
    if let Some(media_kind) = detect_magic(data) {
        return media_kind;
    }
    if let Some(media_kind) = detect_extension(filename) {
        return media_kind;
    }
    if looks_like_text(data) {
        return DetectedMediaKind::Text;
    }
    DetectedMediaKind::Unknown
}

/// Probe both the coarse type and the most specific safe MIME type available.
pub fn probe_media(data: &[u8], filename: &str) -> MediaProbe {
    let media_kind = detect_media_kind(data, filename);
    let detected_mime = detect_mime_type_with_filename(data, filename);
    let mime_type = if detected_mime != "application/octet-stream" {
        detected_mime.to_owned()
    } else if media_kind == DetectedMediaKind::Text {
        "text/plain".to_owned()
    } else {
        detected_mime.to_owned()
    };
    MediaProbe::new(media_kind, mime_type)
}

/// Probe with a browser/provider MIME value as a final fallback only.
/// Content signatures and known filename extensions always take precedence.
pub fn probe_media_with_hint(
    data: &[u8],
    filename: &str,
    hinted_mime_type: Option<&str>,
) -> MediaProbe {
    let detected = probe_media(data, filename);
    if detected.media_kind != DetectedMediaKind::Unknown {
        return detected;
    }
    let Some(hinted) = hinted_mime_type
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return detected;
    };
    let normalized = hinted
        .split(';')
        .next()
        .unwrap_or(hinted)
        .trim()
        .to_ascii_lowercase();
    if normalized.ends_with("/*") {
        return detected;
    }
    let media_kind = media_kind_from_mime_type(&normalized);
    if media_kind == DetectedMediaKind::Unknown {
        return detected;
    }
    MediaProbe::new(media_kind, normalized)
}

/// Guess an exact MIME type from recognizable bytes.
pub fn detect_mime_type(data: &[u8]) -> &'static str {
    if data.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return "image/jpeg";
    }
    if data.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) {
        return "image/png";
    }
    if data.starts_with(b"GIF87a") || data.starts_with(b"GIF89a") {
        return "image/gif";
    }
    if data.starts_with(b"RIFF") {
        if data.len() >= 12 && &data[8..12] == b"WEBP" {
            return "image/webp";
        }
        if data.len() >= 12 && &data[8..12] == b"WAVE" {
            return "audio/wav";
        }
        if data.len() >= 12 && &data[8..12] == b"AVI " {
            return "video/x-msvideo";
        }
    }
    if data.starts_with(b"ID3") {
        return "audio/mpeg";
    }
    if data.len() >= 2 && data[0] == 0xFF && (data[1] & 0xE0) == 0xE0 && (data[1] & 0x18) != 0x08 {
        return "audio/mpeg";
    }
    if data.starts_with(b"fLaC") {
        return "audio/flac";
    }
    if data.starts_with(b"OggS") {
        return "audio/ogg";
    }
    if data.starts_with(b"%PDF") {
        return "application/pdf";
    }
    if data.len() >= 12 && &data[4..8] == b"ftyp" {
        let brand = &data[8..12];
        if matches!(brand, b"M4A " | b"f4a " | b"M4B " | b"M4P ") {
            return "audio/mp4";
        }
        return "video/mp4";
    }
    if data.starts_with(&[0x1A, 0x45, 0xDF, 0xA3]) {
        return "video/webm";
    }
    if data.starts_with(&[0x49, 0x49, 0x2A, 0x00]) || data.starts_with(&[0x4D, 0x4D, 0x00, 0x2A]) {
        return "image/tiff";
    }
    if data.starts_with(b"BM") {
        return "image/bmp";
    }
    "application/octet-stream"
}

/// Detect a MIME type from content, then use the filename as a controlled
/// fallback for formats without a reliable short signature.
pub fn detect_mime_type_with_filename(data: &[u8], filename: &str) -> &'static str {
    let detected = detect_mime_type(data);
    if detected != "application/octet-stream" {
        return detected;
    }
    mime_type_from_extension(filename).unwrap_or(detected)
}

/// Derive the canonical MIME type from a filename extension.
pub fn mime_type_from_extension(filename: &str) -> Option<&'static str> {
    let ext = filename.rsplit('.').next()?.to_ascii_lowercase();
    Some(match ext.as_str() {
        "jpg" | "jpeg" => "image/jpeg",
        "png" => "image/png",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "bmp" => "image/bmp",
        "tiff" | "tif" => "image/tiff",
        "ico" => "image/x-icon",
        "svg" => "image/svg+xml",
        "heic" => "image/heic",
        "avif" => "image/avif",
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        "flac" => "audio/flac",
        "ogg" | "oga" => "audio/ogg",
        "m4a" | "m4b" | "m4p" => "audio/mp4",
        "aac" => "audio/aac",
        "wma" => "audio/x-ms-wma",
        "opus" => "audio/opus",
        "amr" => "audio/amr",
        "mp4" | "m4v" => "video/mp4",
        "webm" => "video/webm",
        "mkv" => "video/x-matroska",
        "avi" => "video/x-msvideo",
        "mov" => "video/quicktime",
        "wmv" => "video/x-ms-wmv",
        "flv" => "video/x-flv",
        "ts" | "mts" => "video/mp2t",
        "3gp" => "video/3gpp",
        "pdf" => "application/pdf",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "pptx" => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        "doc" => "application/msword",
        "xls" => "application/vnd.ms-excel",
        "ppt" => "application/vnd.ms-powerpoint",
        "txt" | "md" | "markdown" | "json" | "csv" | "log" | "xml" | "html" | "htm" | "yaml"
        | "yml" | "toml" | "ini" | "conf" | "cfg" | "py" | "rs" | "js" | "css" | "svelte"
        | "sql" | "tsv" | "vtt" | "srt" | "bat" | "ps1" | "sh" | "c" | "h" | "cpp" | "hpp"
        | "java" | "go" | "rb" | "php" | "lua" | "r" => "text/plain",
        _ => return None,
    })
}

/// Derive a file extension from a MIME type.
pub fn extension_for_mime_type(mime_type: &str) -> &'static str {
    match mime_type.to_ascii_lowercase().as_str() {
        "image/png" => "png",
        "image/jpeg" | "image/jpg" => "jpg",
        "image/webp" => "webp",
        "image/gif" => "gif",
        "image/bmp" => "bmp",
        "image/tiff" => "tiff",
        "image/svg+xml" => "svg",
        "audio/mpeg" => "mp3",
        "audio/wav" => "wav",
        "audio/mp4" => "m4a",
        "audio/ogg" => "ogg",
        "audio/flac" => "flac",
        "audio/aac" => "aac",
        "audio/opus" => "opus",
        "video/mp4" => "mp4",
        "video/webm" => "webm",
        "video/x-msvideo" => "avi",
        "video/quicktime" => "mov",
        _ => "bin",
    }
}

/// Classify an already normalized MIME type without inspecting bytes.
pub fn media_kind_from_mime_type(mime_type: &str) -> DetectedMediaKind {
    let mime_type = mime_type
        .split(';')
        .next()
        .unwrap_or(mime_type)
        .trim()
        .to_ascii_lowercase();
    if mime_type.starts_with("image/") {
        DetectedMediaKind::Image
    } else if mime_type.starts_with("audio/") {
        DetectedMediaKind::Audio
    } else if mime_type.starts_with("video/") {
        DetectedMediaKind::Video
    } else if mime_type == "application/pdf"
        || mime_type.contains("wordprocessingml")
        || mime_type.contains("spreadsheetml")
        || mime_type.contains("presentationml")
        || matches!(
            mime_type.as_str(),
            "application/msword" | "application/vnd.ms-excel" | "application/vnd.ms-powerpoint"
        )
    {
        DetectedMediaKind::Document
    } else if mime_type.starts_with("text/") {
        DetectedMediaKind::Text
    } else {
        DetectedMediaKind::Unknown
    }
}

fn detect_magic(data: &[u8]) -> Option<DetectedMediaKind> {
    if data.starts_with(&[0xFF, 0xD8, 0xFF])
        || data.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A])
        || data.starts_with(b"GIF87a")
        || data.starts_with(b"GIF89a")
        || data.starts_with(b"BM")
        || data.starts_with(&[0x49, 0x49, 0x2A, 0x00])
        || data.starts_with(&[0x4D, 0x4D, 0x00, 0x2A])
        || data.starts_with(&[0x00, 0x00, 0x01, 0x00])
        || (data.len() >= 12 && data.starts_with(b"RIFF") && &data[8..12] == b"WEBP")
    {
        return Some(DetectedMediaKind::Image);
    }
    if data.starts_with(b"ID3")
        || (data.len() >= 2
            && data[0] == 0xFF
            && (data[1] & 0xE0) == 0xE0
            && (data[1] & 0x18) != 0x08)
        || (data.len() >= 12 && data.starts_with(b"RIFF") && &data[8..12] == b"WAVE")
        || data.starts_with(b"fLaC")
        || data.starts_with(b"OggS")
    {
        return Some(DetectedMediaKind::Audio);
    }
    if (data.len() >= 12 && data.starts_with(b"RIFF") && &data[8..12] == b"AVI ")
        || data.starts_with(&[0x1A, 0x45, 0xDF, 0xA3])
        || data.starts_with(&[0x30, 0x26, 0xB2, 0x75, 0x8E, 0x66, 0xCF, 0x11])
    {
        return Some(DetectedMediaKind::Video);
    }
    if data.len() >= 12 && &data[4..8] == b"ftyp" {
        let brand = &data[8..12];
        return Some(if matches!(brand, b"M4A " | b"f4a " | b"M4B " | b"M4P ") {
            DetectedMediaKind::Audio
        } else {
            DetectedMediaKind::Video
        });
    }
    if data.starts_with(b"%PDF") {
        return Some(DetectedMediaKind::Document);
    }
    None
}

fn detect_extension(filename: &str) -> Option<DetectedMediaKind> {
    let mime = mime_type_from_extension(filename)?;
    Some(media_kind_from_mime_type(mime))
}

fn looks_like_text(data: &[u8]) -> bool {
    if data.is_empty() {
        return true;
    }
    if control_ratio(data) > 0.02 {
        return false;
    }
    if std::str::from_utf8(data).is_ok() {
        return true;
    }
    let (_, _, had_errors) = encoding_rs::GBK.decode(data);
    !had_errors
}

fn control_ratio(data: &[u8]) -> f32 {
    if data.is_empty() {
        return 0.0;
    }
    let controls = data
        .iter()
        .filter(|byte| {
            let byte = **byte;
            byte < 0x09 || (byte > 0x0D && byte < 0x20)
        })
        .count();
    controls as f32 / data.len() as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probes_image_audio_and_video() {
        assert_eq!(
            probe_media(b"\x89PNG\r\n\x1a\n", "x.png").media_kind,
            DetectedMediaKind::Image
        );
        assert_eq!(
            probe_media(b"RIFF....WAVE", "x.wav").media_kind,
            DetectedMediaKind::Audio
        );
        assert_eq!(
            probe_media(b"\x00\x00\x00\x18ftypisom", "x.mp4").media_kind,
            DetectedMediaKind::Video
        );
    }

    #[test]
    fn media_probe_serde_uses_the_detected_kind_name() {
        let probe = MediaProbe::new(DetectedMediaKind::Image, "image/png");
        let serialized = serde_json::to_value(&probe).unwrap();
        assert_eq!(serialized["media_kind"], "image");
        assert_eq!(serialized["mime_type"], "image/png");
        assert!(serialized.get("media_type").is_none());

        assert!(
            serde_json::from_value::<MediaProbe>(serde_json::json!({
                "media_type": "image",
                "mime_type": "image/png"
            }))
            .is_err()
        );
    }

    #[test]
    fn filename_fallback_covers_modern_media_formats() {
        for (filename, expected) in [
            ("photo.heic", DetectedMediaKind::Image),
            ("voice.opus", DetectedMediaKind::Audio),
            ("clip.mts", DetectedMediaKind::Video),
            ("sheet.xlsx", DetectedMediaKind::Document),
        ] {
            assert_eq!(
                probe_media(b"not a signature", filename).media_kind,
                expected
            );
        }
    }

    #[test]
    fn content_wins_over_filename_and_hint() {
        let probe = probe_media_with_hint(b"%PDF-1.7", "wrong.png", Some("audio/mpeg"));
        assert_eq!(probe.media_kind, DetectedMediaKind::Document);
        assert_eq!(probe.mime_type, "application/pdf");
    }

    #[test]
    fn unknown_content_can_use_specific_hint() {
        let probe = probe_media_with_hint(&[0, 1, 2, 0xff], "no-extension", Some("video/mp4"));
        assert_eq!(probe.media_kind, DetectedMediaKind::Video);
        assert_eq!(probe.mime_type, "video/mp4");
    }

    #[test]
    fn wildcard_hint_does_not_become_a_canonical_mime() {
        let probe = probe_media_with_hint(&[0, 1, 2, 0xff], "no-extension", Some("audio/*"));
        assert_eq!(probe.media_kind, DetectedMediaKind::Unknown);
        assert_eq!(probe.mime_type, "application/octet-stream");
    }

    #[test]
    fn text_fallback_is_stable() {
        assert_eq!(
            probe_media("你好".as_bytes(), "notes.txt").media_kind,
            DetectedMediaKind::Text
        );
        let (encoded, _, had_errors) = encoding_rs::GBK.encode("中文文本内容");
        assert!(!had_errors);
        assert_eq!(
            probe_media(&encoded, "").media_kind,
            DetectedMediaKind::Text
        );
    }

    #[test]
    fn unknown_binary_stays_unknown() {
        assert_eq!(
            probe_media(&[0, 1, 2, 0xff], "").media_kind,
            DetectedMediaKind::Unknown
        );
    }
}
