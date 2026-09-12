//! Shared media classification, model references, and operation helpers.

use haven_common::media::MediaModality;
use serde_json::{Value, json};
use std::path::Path;

use crate::ManagedAsset;
use crate::document::supports_document_path;

use super::MediaOperation;

/// Coarse media classification shared by the media tool and window output
/// projection. MIME is authoritative when it is specific; the filename is a
/// controlled fallback for restored or loosely typed assets.
pub(crate) fn classify_media(asset: &ManagedAsset) -> (MediaModality, &'static str) {
    let media_type = asset.media_type.to_ascii_lowercase();
    let extension = asset
        .filename
        .as_deref()
        .and_then(|name| Path::new(name).extension())
        .and_then(|ext| ext.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();

    if media_type.starts_with("image/")
        || matches!(
            extension.as_str(),
            "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp"
        )
    {
        return (MediaModality::Image, "image");
    }
    if media_type.starts_with("audio/")
        || matches!(extension.as_str(), "wav" | "mp3" | "flac" | "ogg" | "m4a")
    {
        return (MediaModality::Audio, "audio");
    }
    if media_type == "application/pdf"
        || matches!(
            extension.as_str(),
            "pdf" | "doc" | "docx" | "xls" | "xlsx" | "ppt" | "pptx"
        )
        || media_type.contains("wordprocessingml")
        || media_type.contains("spreadsheetml")
        || media_type.contains("presentationml")
        || matches!(
            media_type.as_str(),
            "application/msword" | "application/vnd.ms-excel" | "application/vnd.ms-powerpoint"
        )
    {
        return (MediaModality::Document, "document");
    }
    if media_type.starts_with("text/") {
        return (MediaModality::Text, "text");
    }
    (MediaModality::Text, "binary")
}

/// Compact model-facing media reference. Runtime-only lifecycle data (hash,
/// expiry, source, size and provider provenance) stays in the host and is not
/// repeated in every tool observation. Every media-producing observation has
/// the same discovery fields, so the model can choose its next operation
/// without knowing which producer created the asset.
pub(crate) fn model_media_reference_with_capabilities(
    asset: &ManagedAsset,
    representation: &str,
    content: Option<&str>,
    describe_available: bool,
    ocr_available: bool,
    transcribe_available: bool,
) -> Value {
    let (modality, file_kind) = classify_media(asset);
    let mut available_representations = vec!["managed_file_ref"];
    match modality {
        MediaModality::Image => {
            if describe_available {
                available_representations.push("image_description");
            }
            if ocr_available {
                available_representations.push("ocr_text");
            }
        }
        MediaModality::Audio if transcribe_available => {
            available_representations.push("transcript");
        }
        MediaModality::Document if supports_document_path(&asset.path) => {
            available_representations.push("document_pages");
        }
        _ => {}
    }
    if !available_representations.contains(&representation) {
        available_representations.push(representation);
    }
    let recommended_next = match (representation, modality) {
        ("managed_file_ref", MediaModality::Image) if describe_available => Some("media.describe"),
        ("managed_file_ref", MediaModality::Image) if ocr_available => Some("media.ocr"),
        ("managed_file_ref", MediaModality::Audio) if transcribe_available => {
            Some("media.transcribe")
        }
        ("managed_file_ref", MediaModality::Document) if supports_document_path(&asset.path) => {
            Some("media.extract")
        }
        _ => None,
    };
    let mut media = json!({
        "asset_id": asset.asset_id.clone(),
        "media_type": asset.media_type.clone(),
        "modality": modality,
        "file_kind": file_kind,
        "representation": representation,
        "available_representations": available_representations,
        "recommended_next": recommended_next,
    });
    if let Some(filename) = asset.filename.as_deref() {
        media["filename"] = json!(filename);
    }
    if let Some(content) = content {
        media["content"] = json!(content);
    }
    media
}

pub(crate) fn bound_text(text: &str, max_chars: usize) -> (String, bool) {
    let bounded: String = text.chars().take(max_chars).collect();
    (bounded, text.chars().count() > max_chars)
}

pub(crate) fn operation_name(operation: MediaOperation) -> &'static str {
    match operation {
        MediaOperation::Inspect => "inspect",
        MediaOperation::Describe => "describe",
        MediaOperation::Ocr => "ocr",
        MediaOperation::Transcribe => "transcribe",
        MediaOperation::Extract => "extract",
        MediaOperation::Generate => "generate",
        MediaOperation::Record => "record",
        MediaOperation::Play => "play",
        MediaOperation::Speak => "speak",
        MediaOperation::VolumeGet => "volume_get",
        MediaOperation::VolumeSet => "volume_set",
        MediaOperation::MuteGet => "mute_get",
        MediaOperation::MuteSet => "mute_set",
    }
}

pub(crate) fn is_audio_operation(operation: MediaOperation) -> bool {
    matches!(
        operation,
        MediaOperation::Record
            | MediaOperation::Play
            | MediaOperation::Speak
            | MediaOperation::VolumeGet
            | MediaOperation::VolumeSet
            | MediaOperation::MuteGet
            | MediaOperation::MuteSet
    )
}

pub(crate) fn confidence_passes(reported: Option<f32>, threshold: f32) -> bool {
    reported
        .map(|confidence| confidence >= threshold)
        .unwrap_or(true)
}
