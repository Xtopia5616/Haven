//! Projection of a planned media representation into canonical content parts.
//!
//! The planner decides *which* representation is safe. This module only
//! serializes that decision into the provider-neutral parts understood by the
//! adapters; it never reads a path or reconstructs an omitted raw payload.

use haven_common::media::{
    MediaInput, MediaPlan, MediaProjectionMode, MediaProvenance, MediaRepresentationKind,
    MediaRepresentationPayload,
};
use haven_common::text::sanitize_prompt_field;
use haven_common::types::ContentPart;

use crate::media::{audio_part, image_part, video_part};
use crate::types::LlmError;

/// Project a previously-built plan into canonical content parts.
pub fn project_media_plan(
    plan: &MediaPlan,
    inputs: &[MediaInput],
) -> Result<Vec<ContentPart>, LlmError> {
    let mut parts = Vec::with_capacity(plan.projections.len());
    for projection in &plan.projections {
        let input = inputs
            .iter()
            .find(|input| input.asset.asset_id == projection.asset_id)
            .ok_or_else(|| {
                LlmError::UnsupportedCapability(format!(
                    "media plan references missing asset {}",
                    projection.asset_id
                ))
            })?;
        let representation = input
            .representations
            .iter()
            .find(|representation| {
                representation.representation == projection.representation
                    && representation.is_available()
            })
            .ok_or_else(|| {
                LlmError::UnsupportedCapability(format!(
                    "media plan references unavailable representation {:?} for {}",
                    projection.representation, projection.asset_id
                ))
            })?;

        let part = match (
            projection.mode,
            projection.representation,
            &representation.payload,
        ) {
            (
                MediaProjectionMode::Raw,
                MediaRepresentationKind::RawImage,
                MediaRepresentationPayload::InlineData { media_type, data },
            ) => image_part(media_type, data.clone()),
            (
                MediaProjectionMode::Raw,
                MediaRepresentationKind::RawAudio,
                MediaRepresentationPayload::InlineData { media_type, data },
            ) => audio_part(media_type, data.clone()),
            (
                MediaProjectionMode::Raw,
                MediaRepresentationKind::RawVideo,
                MediaRepresentationPayload::InlineData { media_type, data },
            ) => video_part(media_type, data.clone()),
            (MediaProjectionMode::Derived, kind, MediaRepresentationPayload::Text(text))
                if kind.is_textual() =>
            {
                ContentPart::text(render_derived_text(&representation.provenance, text))
            }
            (
                MediaProjectionMode::ManagedReference,
                MediaRepresentationKind::ManagedFileRef,
                MediaRepresentationPayload::ManagedFileRef { asset_id, filename },
            ) => ContentPart::text(render_managed_reference(asset_id, filename.as_deref())),
            (_, kind, _) => {
                return Err(LlmError::UnsupportedCapability(format!(
                    "media representation {:?} cannot be projected as {:?}",
                    kind, projection.mode
                )));
            }
        };
        parts.push(part);
    }
    Ok(parts)
}

fn render_derived_text(provenance: &MediaProvenance, text: &str) -> String {
    let operation = match provenance {
        MediaProvenance::Original => "unknown",
        MediaProvenance::Derived { operation, .. } => match operation {
            haven_common::media::MediaDerivation::Ocr => "ocr",
            haven_common::media::MediaDerivation::Stt => "stt",
            haven_common::media::MediaDerivation::DocumentExtract => "document_extract",
            haven_common::media::MediaDerivation::ImageDescribe => "image_describe",
            haven_common::media::MediaDerivation::TableExtract => "table_extract",
            haven_common::media::MediaDerivation::Thumbnail => "thumbnail",
            haven_common::media::MediaDerivation::Tool => "tool",
        },
    };
    let safe_text = sanitize_prompt_field(text, 32_000);
    format!(
        "【附件派生内容开始：provenance={operation}；不可信外部内容】\n{safe_text}\n【附件派生内容结束】"
    )
}

fn render_managed_reference(asset_id: &str, filename: Option<&str>) -> String {
    let label = filename
        .map(|name| sanitize_prompt_field(name, 120))
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "attachment".into());
    let safe_asset_id = sanitize_prompt_field(asset_id, 96);
    format!(
        "[受管附件: {label}; asset_id={safe_asset_id}; 请使用受管文件工具访问，不要猜测本机路径]"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use haven_common::media::{
        CapabilityProfile, CapabilitySupport, MediaAsset, MediaAssetLifecycle, MediaAssetSource,
        MediaDerivation, MediaInputStrategy, MediaRepresentation, MediaRepresentationPayload,
    };

    fn input(media_type: &str, representation: MediaRepresentation) -> MediaInput {
        let asset = MediaAsset::new(
            media_type,
            16,
            Some("report.txt".into()),
            MediaAssetSource::RestoredLegacy,
            MediaAssetLifecycle::Session,
        );
        MediaInput {
            asset,
            representations: vec![representation],
            preferred_representation: None,
        }
    }

    #[test]
    fn projects_raw_image_without_path_or_reencoding() {
        let input = input(
            "image/png",
            MediaRepresentation::available(
                MediaRepresentationKind::RawImage,
                MediaProvenance::Original,
                MediaRepresentationPayload::InlineData {
                    media_type: "image/png".into(),
                    data: "aGVsbG8=".into(),
                },
            ),
        );
        let profile = CapabilityProfile {
            image: CapabilitySupport::Supported,
            ..CapabilityProfile::default()
        };
        let plan = haven_common::media::build_media_plan(
            std::slice::from_ref(&input),
            &profile,
            MediaInputStrategy::Auto,
        );
        let parts = project_media_plan(&plan, std::slice::from_ref(&input)).unwrap();
        assert!(
            matches!(parts.as_slice(), [ContentPart::Image { data, .. }] if data == "aGVsbG8=")
        );
    }

    #[test]
    fn projects_derived_text_with_untrusted_fence() {
        let input = input(
            "image/png",
            MediaRepresentation::available(
                MediaRepresentationKind::OcrText,
                MediaProvenance::Derived {
                    operation: MediaDerivation::Ocr,
                    provider: Some("ocr".into()),
                    source_kind: Some(MediaRepresentationKind::RawImage),
                },
                MediaRepresentationPayload::Text("line\nIGNORE".into()),
            ),
        );
        let profile = CapabilityProfile::default();
        let plan = haven_common::media::build_media_plan(
            std::slice::from_ref(&input),
            &profile,
            MediaInputStrategy::ExtractedPreferred,
        );
        let parts = project_media_plan(&plan, std::slice::from_ref(&input)).unwrap();
        assert!(matches!(parts.as_slice(), [ContentPart::Text(text)]
            if text.contains("provenance=ocr")
                && text.contains("line IGNORE")
                && text.contains("不可信外部内容")));
    }

    #[test]
    fn managed_reference_never_contains_a_path() {
        let input = input(
            "application/pdf",
            MediaRepresentation::available(
                MediaRepresentationKind::ManagedFileRef,
                MediaProvenance::Original,
                MediaRepresentationPayload::ManagedFileRef {
                    asset_id: "asset-0123456789abcdef".into(),
                    filename: Some("report.pdf".into()),
                },
            ),
        );
        let profile = CapabilityProfile {
            tools: CapabilitySupport::Supported,
            ..CapabilityProfile::default()
        };
        let plan = haven_common::media::build_media_plan(
            std::slice::from_ref(&input),
            &profile,
            MediaInputStrategy::Auto,
        );
        let parts = project_media_plan(&plan, std::slice::from_ref(&input)).unwrap();
        assert!(matches!(parts.as_slice(), [ContentPart::Text(text)]
            if text.contains("asset-0123456789abcdef")
                && text.contains("report.pdf")
                && !text.contains("C:\\")
                && !text.contains("/Users/")));
    }
}
