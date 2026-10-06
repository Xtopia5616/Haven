//! Canonical classification adapter for filesystem-facing tools.

use haven_common::media_detection::{
    DetectedMediaKind, media_kind_from_mime_type, mime_type_from_extension,
};
use std::path::Path;

/// Classify a file by its extension into the coarse file kinds used by
/// `files.read`. Rich media MIME values come exclusively from the common
/// detector; archive and executable kinds remain filesystem-only categories.
pub(super) fn classify_by_extension(path: &str) -> (&'static str, &'static str) {
    if let Some(mime) = mime_type_from_extension(path) {
        match media_kind_from_mime_type(mime) {
            DetectedMediaKind::Image => return ("image", mime),
            DetectedMediaKind::Audio => return ("audio", mime),
            DetectedMediaKind::Video => return ("video", mime),
            DetectedMediaKind::Document => {
                return if mime == "application/pdf" {
                    ("pdf", mime)
                } else {
                    ("office", mime)
                };
            }
            DetectedMediaKind::Text | DetectedMediaKind::Unknown => {}
        }
    }
    let ext = Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "zip" => ("archive", "application/zip"),
        "7z" => ("archive", "application/x-7z-compressed"),
        "tar" | "gz" | "tgz" => ("archive", "application/x-tar"),
        "rar" => ("archive", "application/vnd.rar"),
        "exe" | "msi" | "dll" => ("executable", "application/octet-stream"),
        _ => ("unknown", "application/octet-stream"),
    }
}
