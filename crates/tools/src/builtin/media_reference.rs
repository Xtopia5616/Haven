//! Shared media classification, model references, and operation helpers.

use haven_common::media::{MediaReference, MediaRepresentationKind, MediaResult};
use haven_common::media_detection::{
    DetectedMediaKind, media_kind_from_mime_type, mime_type_from_extension,
};
use serde_json::Value;

use crate::ManagedAsset;
use crate::document::supports_document_path;

use super::MediaCapabilities;
use super::MediaOperation;

/// Coarse media classification shared by the media tool and window output
/// projection. MIME is authoritative when it is specific; the filename is a
/// controlled fallback for restored or loosely typed assets.
pub(crate) fn classify_media(asset: &ManagedAsset) -> (DetectedMediaKind, &'static str) {
    let detected = match media_kind_from_mime_type(&asset.media_type) {
        DetectedMediaKind::Unknown => None,
        media_kind => Some(media_kind),
    }
    .or_else(|| {
        asset
            .filename
            .as_deref()
            .and_then(mime_type_from_extension)
            .map(media_kind_from_mime_type)
    })
    .unwrap_or(DetectedMediaKind::Unknown);

    match detected {
        DetectedMediaKind::Image => (DetectedMediaKind::Image, "image"),
        DetectedMediaKind::Audio => (DetectedMediaKind::Audio, "audio"),
        DetectedMediaKind::Video => (DetectedMediaKind::Video, "video"),
        DetectedMediaKind::Document => (DetectedMediaKind::Document, "document"),
        DetectedMediaKind::Text => (DetectedMediaKind::Text, "text"),
        DetectedMediaKind::Unknown => (DetectedMediaKind::Unknown, "binary"),
    }
}

/// Compact model-facing media reference. Runtime-only lifecycle data (hash,
/// expiry, source, size and provider provenance) stays in the host and is not
/// repeated in every tool observation. Every media-producing observation has
/// the same discovery fields, so the model can choose its next operation
/// without knowing which producer created the asset.
#[cfg(test)]
pub(crate) fn model_media_reference_with_capabilities(
    asset: &ManagedAsset,
    representation: MediaRepresentationKind,
    content: Option<&str>,
    capabilities: MediaCapabilities,
) -> Value {
    serde_json::to_value(media_reference_with_capabilities(
        asset,
        representation,
        content,
        capabilities,
    ))
    .expect("media reference is serializable")
}

pub(crate) fn media_reference_with_capabilities(
    asset: &ManagedAsset,
    representation: MediaRepresentationKind,
    content: Option<&str>,
    capabilities: MediaCapabilities,
) -> MediaReference {
    let (media_kind, file_kind) = classify_media(asset);
    let mut available_representations = vec![MediaRepresentationKind::ManagedFileRef];
    match media_kind {
        DetectedMediaKind::Image => {
            if capabilities.describe {
                available_representations.push(MediaRepresentationKind::ImageDescription);
            }
            if capabilities.ocr {
                available_representations.push(MediaRepresentationKind::OcrText);
            }
        }
        DetectedMediaKind::Audio if capabilities.transcribe => {
            available_representations.push(MediaRepresentationKind::Transcript);
        }
        DetectedMediaKind::Document if supports_document_path(&asset.path) => {
            available_representations.push(MediaRepresentationKind::DocumentPages);
        }
        _ => {}
    }
    if !available_representations.contains(&representation) {
        available_representations.push(representation);
    }
    let recommended_next = match (representation, media_kind) {
        (MediaRepresentationKind::ManagedFileRef, DetectedMediaKind::Image)
            if capabilities.describe =>
        {
            Some("media.describe")
        }
        (MediaRepresentationKind::ManagedFileRef, DetectedMediaKind::Image) if capabilities.ocr => {
            Some("media.ocr")
        }
        (MediaRepresentationKind::ManagedFileRef, DetectedMediaKind::Audio)
            if capabilities.transcribe =>
        {
            Some("media.transcribe")
        }
        (MediaRepresentationKind::ManagedFileRef, DetectedMediaKind::Video) => None,
        (MediaRepresentationKind::ManagedFileRef, DetectedMediaKind::Document)
            if supports_document_path(&asset.path) =>
        {
            Some("media.extract")
        }
        _ => None,
    };
    MediaReference {
        asset_id: asset.asset_id.clone(),
        media_type: asset.media_type.clone(),
        modality: media_kind,
        file_kind: file_kind.to_owned(),
        representation,
        available_representations,
        recommended_next: recommended_next.map(str::to_owned),
        filename: asset.filename.clone(),
        content: content.map(str::to_owned),
    }
}

/// Build the shared outer media result without exposing host-owned paths.
pub(crate) fn media_result_envelope(
    operation: MediaOperation,
    asset: Option<&ManagedAsset>,
    representation: Option<MediaRepresentationKind>,
    content: Option<&str>,
    capabilities: MediaCapabilities,
) -> Value {
    media_result_envelope_named(
        operation_name(operation),
        asset,
        representation,
        content,
        capabilities,
    )
}

pub(crate) fn media_result_envelope_named(
    operation: impl Into<String>,
    asset: Option<&ManagedAsset>,
    representation: Option<MediaRepresentationKind>,
    content: Option<&str>,
    capabilities: MediaCapabilities,
) -> Value {
    let result = if let Some(asset) = asset {
        MediaResult::asset(
            operation,
            media_reference_with_capabilities(
                asset,
                representation.unwrap_or(MediaRepresentationKind::ManagedFileRef),
                content,
                capabilities,
            ),
            representation,
        )
    } else {
        MediaResult::device(operation)
    };
    serde_json::to_value(result).expect("media result is serializable")
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
        MediaOperation::Render => "render",
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

pub(crate) fn confidence_passes(reported: Option<f32>, threshold: f32) -> bool {
    reported
        .map(|confidence| confidence >= threshold)
        .unwrap_or(true)
}
