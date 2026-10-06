//! Canonical classification adapter for filesystem-facing tools.

use haven_common::media_detection::{
    DetectedMediaKind, media_kind_from_mime_type, mime_type_from_extension,
};
use std::path::Path;

/// File classes used by the `files` operations. These include handling
/// categories such as archives and executables that are not media kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum FileClassificationKind {
    Image,
    Audio,
    Video,
    Pdf,
    Office,
    Archive,
    Executable,
    Unknown,
}

impl FileClassificationKind {
    pub(super) const fn as_str(self) -> &'static str {
        match self {
            Self::Image => "image",
            Self::Audio => "audio",
            Self::Video => "video",
            Self::Pdf => "pdf",
            Self::Office => "office",
            Self::Archive => "archive",
            Self::Executable => "executable",
            Self::Unknown => "unknown",
        }
    }
}

/// File handling category and its MIME type, classified from the filename.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct FileClassification {
    pub file_kind: FileClassificationKind,
    pub mime_type: &'static str,
}

/// Classify a file by its extension into the file kinds used by `files.read`.
/// Rich-media MIME values come from Common; archive and executable kinds are
/// filesystem-facing categories owned here.
pub(super) fn classify_file_by_extension(path: &str) -> FileClassification {
    if let Some(mime_type) = mime_type_from_extension(path) {
        match media_kind_from_mime_type(mime_type) {
            DetectedMediaKind::Image => {
                return FileClassification {
                    file_kind: FileClassificationKind::Image,
                    mime_type,
                };
            }
            DetectedMediaKind::Audio => {
                return FileClassification {
                    file_kind: FileClassificationKind::Audio,
                    mime_type,
                };
            }
            DetectedMediaKind::Video => {
                return FileClassification {
                    file_kind: FileClassificationKind::Video,
                    mime_type,
                };
            }
            DetectedMediaKind::Document => {
                let kind = if mime_type == "application/pdf" {
                    FileClassificationKind::Pdf
                } else {
                    FileClassificationKind::Office
                };
                return FileClassification {
                    file_kind: kind,
                    mime_type,
                };
            }
            DetectedMediaKind::Text | DetectedMediaKind::Unknown => {}
        }
    }

    let extension = Path::new(path)
        .extension()
        .and_then(|extension| extension.to_str())
        .map(|extension| extension.to_ascii_lowercase())
        .unwrap_or_default();
    let (kind, mime_type) = match extension.as_str() {
        "zip" => (FileClassificationKind::Archive, "application/zip"),
        "7z" => (
            FileClassificationKind::Archive,
            "application/x-7z-compressed",
        ),
        "tar" | "gz" | "tgz" => (FileClassificationKind::Archive, "application/x-tar"),
        "rar" => (FileClassificationKind::Archive, "application/vnd.rar"),
        "exe" | "msi" | "dll" => (
            FileClassificationKind::Executable,
            "application/octet-stream",
        ),
        _ => (FileClassificationKind::Unknown, "application/octet-stream"),
    };
    FileClassification {
        file_kind: kind,
        mime_type,
    }
}
