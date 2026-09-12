//! Canonical classification adapter for filesystem-facing tools.

use haven_common::media_detection::{MediaType, media_type_from_extension, media_type_from_mime};
use std::path::Path;

/// Classify a file by its extension into the coarse file kinds used by
/// `files.read`. Rich media MIME values come exclusively from the common
/// detector; archive and executable kinds remain filesystem-only categories.
pub(super) fn classify_by_extension(path: &str) -> (&'static str, &'static str) {
    if let Some(mime) = media_type_from_extension(path) {
        match media_type_from_mime(mime) {
            MediaType::Image => return ("image", mime),
            MediaType::Audio => return ("audio", mime),
            MediaType::Video => return ("video", mime),
            MediaType::Document => {
                return if mime == "application/pdf" {
                    ("pdf", mime)
                } else {
                    ("office", mime)
                };
            }
            MediaType::Text | MediaType::Unknown => {}
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
