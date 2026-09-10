//! Provider-neutral media assets, representations and request planning.
//!
//! This module deliberately contains no file I/O, provider names or Tauri
//! types. It is the stable contract shared by ingress, the ReAct layer and
//! provider-facing adapters. The planner chooses *which* representation is
//! safe to expose; a later boundary is responsible for serializing that
//! representation into a provider wire format.

use crate::types::new_id;
use serde::{Deserialize, Serialize};

/// Runtime/storage identifier for a managed media asset.
pub const MEDIA_ASSET_ID_PREFIX: &str = "asset";

/// A safe, provider-neutral description of one managed or referenced asset.
///
/// `content_hash` may be empty while a legacy attachment is being adapted;
/// new host-created assets should fill it with a lowercase SHA-256 hex digest.
/// Raw bytes and absolute paths intentionally do not belong here.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MediaAsset {
    pub asset_id: String,
    pub content_hash: String,
    pub media_type: String,
    pub size_bytes: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filename: Option<String>,
    pub source: MediaAssetSource,
    pub lifecycle: MediaAssetLifecycle,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
}

impl MediaAsset {
    pub fn new(
        media_type: impl Into<String>,
        size_bytes: u64,
        filename: Option<String>,
        source: MediaAssetSource,
        lifecycle: MediaAssetLifecycle,
    ) -> Self {
        Self {
            asset_id: new_id(MEDIA_ASSET_ID_PREFIX),
            content_hash: String::new(),
            media_type: media_type.into(),
            size_bytes,
            filename,
            source,
            lifecycle,
            expires_at: None,
        }
    }

    pub fn with_hash(mut self, content_hash: impl Into<String>) -> Self {
        self.content_hash = content_hash.into();
        self
    }
}

/// Where an asset entered Haven's media pipeline.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MediaAssetSource {
    UserAttachment,
    Recording,
    WindowCapture,
    Generated,
    ToolOutput,
    RestoredLegacy,
}

/// Retention boundary for an asset. The concrete cleanup owner is introduced
/// by the managed-file stage; this enum lets planning code reason about the
/// boundary without knowing a filesystem path.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MediaAssetLifecycle {
    Request,
    Session,
    Managed,
    External,
}

/// A representation kind that can be selected for a provider request.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum MediaRepresentationKind {
    RawImage,
    RawAudio,
    RawVideo,
    ExtractedText,
    Transcript,
    OcrText,
    ImageDescription,
    DocumentPages,
    TableData,
    Thumbnail,
    ManagedFileRef,
}

impl MediaRepresentationKind {
    pub const fn is_raw(self) -> bool {
        matches!(
            self,
            Self::RawImage | Self::RawAudio | Self::RawVideo | Self::Thumbnail
        )
    }

    pub const fn is_textual(self) -> bool {
        matches!(
            self,
            Self::ExtractedText
                | Self::Transcript
                | Self::OcrText
                | Self::ImageDescription
                | Self::DocumentPages
                | Self::TableData
        )
    }

    pub const fn is_managed_reference(self) -> bool {
        matches!(self, Self::ManagedFileRef)
    }

    pub const fn raw_modality(self) -> Option<MediaModality> {
        match self {
            Self::RawImage | Self::Thumbnail => Some(MediaModality::Image),
            Self::RawAudio => Some(MediaModality::Audio),
            Self::RawVideo => Some(MediaModality::Video),
            _ => None,
        }
    }
}

/// Modality used by the capability contract. This is intentionally separate
/// from the LLM gateway's detection enum so common types do not depend on
/// provider routing code.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum MediaModality {
    Text,
    Image,
    Audio,
    Video,
    Document,
}

/// Whether a representation is original user input or a derived result.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum MediaProvenance {
    Original,
    Derived {
        operation: MediaDerivation,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        source_kind: Option<MediaRepresentationKind>,
    },
}

/// Operation that produced a derived representation.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MediaDerivation {
    Ocr,
    Stt,
    DocumentExtract,
    ImageDescribe,
    TableExtract,
    Thumbnail,
    Tool,
}

/// Availability is explicit so a pending/failed derivation cannot be
/// mistaken for an empty successful representation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "state")]
pub enum MediaRepresentationAvailability {
    Available,
    Pending,
    Unavailable { reason: String },
}

/// Optional accounting attached to a representation. It is metadata only;
/// the planner does not perform provider calls to fill it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MediaRepresentationCost {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub estimated_tokens: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub estimated_usd: Option<f64>,
}

/// Representation payload. Managed references contain only the opaque asset
/// id and display name; an absolute path is resolved by a trusted tool/host
/// boundary and never becomes provider-facing text here.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case", tag = "kind", content = "value")]
pub enum MediaRepresentationPayload {
    InlineData {
        media_type: String,
        data: String,
    },
    Text(String),
    Structured(serde_json::Value),
    ManagedFileRef {
        asset_id: String,
        filename: Option<String>,
    },
}

/// One available view of an asset.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MediaRepresentation {
    pub representation: MediaRepresentationKind,
    pub provenance: MediaProvenance,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost: Option<MediaRepresentationCost>,
    pub availability: MediaRepresentationAvailability,
    pub payload: MediaRepresentationPayload,
}

impl MediaRepresentation {
    pub fn available(
        representation: MediaRepresentationKind,
        provenance: MediaProvenance,
        payload: MediaRepresentationPayload,
    ) -> Self {
        Self {
            representation,
            provenance,
            confidence: None,
            cost: None,
            availability: MediaRepresentationAvailability::Available,
            payload,
        }
    }

    pub fn unavailable(
        representation: MediaRepresentationKind,
        provenance: MediaProvenance,
        reason: impl Into<String>,
    ) -> Self {
        Self {
            representation,
            provenance,
            confidence: None,
            cost: None,
            availability: MediaRepresentationAvailability::Unavailable {
                reason: reason.into(),
            },
            payload: MediaRepresentationPayload::Text(String::new()),
        }
    }

    pub fn is_available(&self) -> bool {
        matches!(
            self.availability,
            MediaRepresentationAvailability::Available
        )
    }
}

/// Tri-state model capability. Unknown is intentionally not treated as
/// supported by the planner.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CapabilitySupport {
    Supported,
    Unsupported,
    Unknown,
}

impl CapabilitySupport {
    pub const fn is_supported(self) -> bool {
        matches!(self, Self::Supported)
    }
}

/// Provider/model capability profile consumed by the pure media planner.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct CapabilityProfile {
    pub text: CapabilitySupport,
    pub image: CapabilitySupport,
    pub audio: CapabilitySupport,
    pub video: CapabilitySupport,
    pub native_file_upload: CapabilitySupport,
    pub tools: CapabilitySupport,
    pub accepted_mime_types: Vec<String>,
    pub max_input_parts: Option<usize>,
    pub max_input_bytes: Option<u64>,
    pub max_context_tokens: Option<u32>,
}

impl Default for CapabilityProfile {
    fn default() -> Self {
        Self {
            text: CapabilitySupport::Supported,
            image: CapabilitySupport::Unknown,
            audio: CapabilitySupport::Unknown,
            video: CapabilitySupport::Unknown,
            native_file_upload: CapabilitySupport::Unknown,
            tools: CapabilitySupport::Unknown,
            accepted_mime_types: Vec::new(),
            max_input_parts: None,
            max_input_bytes: None,
            max_context_tokens: None,
        }
    }
}

impl CapabilityProfile {
    pub fn supports(
        &self,
        representation: &MediaRepresentation,
        asset: &MediaAsset,
    ) -> CapabilitySupport {
        let support = match representation.representation {
            kind if kind.is_textual() => self.text,
            MediaRepresentationKind::ManagedFileRef => {
                or_support(self.native_file_upload, self.tools)
            }
            kind => match kind.raw_modality() {
                Some(MediaModality::Image) => self.image,
                Some(MediaModality::Audio) => self.audio,
                Some(MediaModality::Video) => self.video,
                _ => CapabilitySupport::Unknown,
            },
        };
        if !support.is_supported() {
            return support;
        }
        if representation.representation.is_raw()
            && !self.accepted_mime_types.is_empty()
            && !self
                .accepted_mime_types
                .iter()
                .any(|accepted| mime_matches(accepted, &asset.media_type))
        {
            return CapabilitySupport::Unsupported;
        }
        support
    }

    pub fn within_size_limit(&self, asset: &MediaAsset) -> bool {
        self.max_input_bytes
            .is_none_or(|limit| asset.size_bytes <= limit)
    }
}

fn or_support(left: CapabilitySupport, right: CapabilitySupport) -> CapabilitySupport {
    match (left, right) {
        (CapabilitySupport::Supported, _) | (_, CapabilitySupport::Supported) => {
            CapabilitySupport::Supported
        }
        (CapabilitySupport::Unknown, _) | (_, CapabilitySupport::Unknown) => {
            CapabilitySupport::Unknown
        }
        _ => CapabilitySupport::Unsupported,
    }
}

fn mime_matches(accepted: &str, actual: &str) -> bool {
    accepted == actual
        || accepted.strip_suffix("/*").is_some_and(|prefix| {
            actual.starts_with(prefix) && actual[prefix.len()..].starts_with('/')
        })
}

/// User-selectable media input policy.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum MediaInputStrategy {
    #[default]
    Auto,
    RawPreferred,
    ExtractedPreferred,
    TextOnlySafe,
}

impl MediaInputStrategy {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::RawPreferred => "raw_preferred",
            Self::ExtractedPreferred => "extracted_preferred",
            Self::TextOnlySafe => "text_only_safe",
        }
    }

    pub fn parse(value: &str) -> Self {
        match value.trim().to_ascii_lowercase().as_str() {
            "raw_preferred" => Self::RawPreferred,
            "extracted_preferred" => Self::ExtractedPreferred,
            "text_only_safe" => Self::TextOnlySafe,
            _ => Self::Auto,
        }
    }
}

/// An asset plus all representations currently available to the planner.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MediaInput {
    pub asset: MediaAsset,
    #[serde(default)]
    pub representations: Vec<MediaRepresentation>,
}

/// What a provider-neutral request projection will expose for one asset.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MediaProjectionMode {
    Raw,
    Derived,
    ManagedReference,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MediaProjection {
    pub asset_id: String,
    pub representation: MediaRepresentationKind,
    pub mode: MediaProjectionMode,
    pub provenance: MediaProvenance,
}

/// Why a plan had to fall back or omit an input. The enum is intentionally
/// stable enough for UI diagnostics; provider-specific error text stays out.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MediaPlanNoticeCode {
    RawCapabilityUnsupported,
    RawCapabilityUnknown,
    RawMimeUnsupported,
    RawSizeExceeded,
    DerivedFallback,
    ManagedReferenceFallback,
    StrategyExcluded,
    InputPartLimit,
    NoCompatibleRepresentation,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MediaPlanNotice {
    pub asset_id: String,
    pub code: MediaPlanNoticeCode,
}

/// Deterministic result of planning one provider request.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MediaPlan {
    pub strategy: MediaInputStrategy,
    pub projections: Vec<MediaProjection>,
    pub notices: Vec<MediaPlanNotice>,
}

impl MediaPlan {
    pub fn is_empty(&self) -> bool {
        self.projections.is_empty()
    }
}

/// Select at most one representation per input asset for this request.
///
/// This function is pure and conservative: unknown raw capabilities are
/// never sent, a text-only policy cannot select raw media or references, and
/// an input exceeding the configured aggregate byte/part budget is omitted
/// with a stable diagnostic code.
pub fn build_media_plan(
    inputs: &[MediaInput],
    capabilities: &CapabilityProfile,
    strategy: MediaInputStrategy,
) -> MediaPlan {
    let mut projections = Vec::new();
    let mut notices = Vec::new();
    let mut used_bytes = 0u64;

    for input in inputs {
        let raw_candidates: Vec<_> = input
            .representations
            .iter()
            .filter(|representation| representation.representation.is_raw())
            .collect();
        let derived_candidates: Vec<_> = input
            .representations
            .iter()
            .filter(|representation| representation.representation.is_textual())
            .collect();
        let managed_candidates: Vec<_> = input
            .representations
            .iter()
            .filter(|representation| representation.representation.is_managed_reference())
            .collect();

        let mut selected: Option<(&MediaRepresentation, MediaProjectionMode)> = None;

        if strategy != MediaInputStrategy::TextOnlySafe
            && matches!(
                strategy,
                MediaInputStrategy::Auto | MediaInputStrategy::RawPreferred
            )
        {
            selected = select_raw(
                raw_candidates.iter().copied(),
                input,
                capabilities,
                &mut notices,
            );
        }

        if selected.is_none() {
            selected = select_derived(derived_candidates.iter().copied(), input, capabilities);
            if selected.is_some()
                && strategy != MediaInputStrategy::ExtractedPreferred
                && input
                    .representations
                    .iter()
                    .any(|representation| representation.representation.is_raw())
            {
                notices.push(MediaPlanNotice {
                    asset_id: input.asset.asset_id.clone(),
                    code: MediaPlanNoticeCode::DerivedFallback,
                });
            }
        }

        // `extracted_preferred` is a preference, not a prohibition. If no
        // safe derived representation is available, fall back to a raw part
        // that the capability profile explicitly supports.
        if selected.is_none() && strategy == MediaInputStrategy::ExtractedPreferred {
            selected = select_raw(
                raw_candidates.iter().copied(),
                input,
                capabilities,
                &mut notices,
            );
        }

        if selected.is_none() && strategy != MediaInputStrategy::TextOnlySafe {
            selected = select_managed(managed_candidates.iter().copied(), input, capabilities);
            if selected.is_some() {
                notices.push(MediaPlanNotice {
                    asset_id: input.asset.asset_id.clone(),
                    code: MediaPlanNoticeCode::ManagedReferenceFallback,
                });
            }
        }

        let Some((representation, mode)) = selected else {
            notices.push(MediaPlanNotice {
                asset_id: input.asset.asset_id.clone(),
                code: if strategy == MediaInputStrategy::TextOnlySafe {
                    MediaPlanNoticeCode::StrategyExcluded
                } else {
                    MediaPlanNoticeCode::NoCompatibleRepresentation
                },
            });
            continue;
        };

        if capabilities
            .max_input_parts
            .is_some_and(|limit| projections.len() >= limit)
        {
            notices.push(MediaPlanNotice {
                asset_id: input.asset.asset_id.clone(),
                code: MediaPlanNoticeCode::InputPartLimit,
            });
            continue;
        }
        let counts_bytes = matches!(
            mode,
            MediaProjectionMode::Raw | MediaProjectionMode::ManagedReference
        );
        if counts_bytes && !capabilities.within_size_limit(&input.asset) {
            notices.push(MediaPlanNotice {
                asset_id: input.asset.asset_id.clone(),
                code: MediaPlanNoticeCode::RawSizeExceeded,
            });
            continue;
        }
        if counts_bytes {
            if let Some(limit) = capabilities.max_input_bytes
                && used_bytes.saturating_add(input.asset.size_bytes) > limit
            {
                notices.push(MediaPlanNotice {
                    asset_id: input.asset.asset_id.clone(),
                    code: MediaPlanNoticeCode::RawSizeExceeded,
                });
                continue;
            }
            used_bytes = used_bytes.saturating_add(input.asset.size_bytes);
        }

        projections.push(MediaProjection {
            asset_id: input.asset.asset_id.clone(),
            representation: representation.representation,
            mode,
            provenance: representation.provenance.clone(),
        });
    }

    MediaPlan {
        strategy,
        projections,
        notices,
    }
}

fn select_raw<'a>(
    candidates: impl Iterator<Item = &'a MediaRepresentation>,
    input: &'a MediaInput,
    capabilities: &CapabilityProfile,
    notices: &mut Vec<MediaPlanNotice>,
) -> Option<(&'a MediaRepresentation, MediaProjectionMode)> {
    for representation in candidates {
        if !representation.is_available() {
            continue;
        }
        match capabilities.supports(representation, &input.asset) {
            CapabilitySupport::Supported => {
                return Some((representation, MediaProjectionMode::Raw));
            }
            CapabilitySupport::Unsupported => notices.push(MediaPlanNotice {
                asset_id: input.asset.asset_id.clone(),
                code: if !capabilities.accepted_mime_types.is_empty()
                    && !capabilities
                        .accepted_mime_types
                        .iter()
                        .any(|accepted| mime_matches(accepted, &input.asset.media_type))
                {
                    MediaPlanNoticeCode::RawMimeUnsupported
                } else {
                    MediaPlanNoticeCode::RawCapabilityUnsupported
                },
            }),
            CapabilitySupport::Unknown => notices.push(MediaPlanNotice {
                asset_id: input.asset.asset_id.clone(),
                code: MediaPlanNoticeCode::RawCapabilityUnknown,
            }),
        }
    }
    None
}

fn select_derived<'a>(
    candidates: impl Iterator<Item = &'a MediaRepresentation>,
    input: &'a MediaInput,
    capabilities: &CapabilityProfile,
) -> Option<(&'a MediaRepresentation, MediaProjectionMode)> {
    // Prefer semantically specific outputs over a generic extraction result.
    let priority = [
        MediaRepresentationKind::Transcript,
        MediaRepresentationKind::OcrText,
        MediaRepresentationKind::ExtractedText,
        MediaRepresentationKind::ImageDescription,
        MediaRepresentationKind::DocumentPages,
        MediaRepresentationKind::TableData,
    ];
    let available: Vec<_> = candidates
        .filter(|representation| representation.is_available())
        .collect();
    for kind in priority {
        let Some(representation) = available
            .iter()
            .copied()
            .find(|representation| representation.representation == kind)
        else {
            continue;
        };
        if capabilities
            .supports(representation, &input.asset)
            .is_supported()
        {
            return Some((representation, MediaProjectionMode::Derived));
        }
    }
    None
}

fn select_managed<'a>(
    candidates: impl Iterator<Item = &'a MediaRepresentation>,
    input: &'a MediaInput,
    capabilities: &CapabilityProfile,
) -> Option<(&'a MediaRepresentation, MediaProjectionMode)> {
    candidates
        .filter(|representation| representation.is_available())
        .find(|representation| {
            capabilities
                .supports(representation, &input.asset)
                .is_supported()
        })
        .map(|representation| (representation, MediaProjectionMode::ManagedReference))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn asset(media_type: &str, size_bytes: u64) -> MediaAsset {
        MediaAsset::new(
            media_type,
            size_bytes,
            Some("upload.bin".into()),
            MediaAssetSource::UserAttachment,
            MediaAssetLifecycle::Session,
        )
        .with_hash("a".repeat(64))
    }

    fn raw(kind: MediaRepresentationKind, media_type: &str) -> MediaRepresentation {
        MediaRepresentation::available(
            kind,
            MediaProvenance::Original,
            MediaRepresentationPayload::InlineData {
                media_type: media_type.into(),
                data: "AQI=".into(),
            },
        )
    }

    fn derived(kind: MediaRepresentationKind, text: &str) -> MediaRepresentation {
        MediaRepresentation::available(
            kind,
            MediaProvenance::Derived {
                operation: match kind {
                    MediaRepresentationKind::Transcript => MediaDerivation::Stt,
                    MediaRepresentationKind::OcrText => MediaDerivation::Ocr,
                    _ => MediaDerivation::DocumentExtract,
                },
                provider: Some("test".into()),
                source_kind: Some(MediaRepresentationKind::RawImage),
            },
            MediaRepresentationPayload::Text(text.into()),
        )
    }

    fn input(asset: MediaAsset, representations: Vec<MediaRepresentation>) -> MediaInput {
        MediaInput {
            asset,
            representations,
        }
    }

    #[test]
    fn asset_ids_use_the_project_id_generator_and_metadata_has_no_path() {
        let asset = asset("image/png", 2);
        assert!(asset.asset_id.starts_with("asset-"));
        assert_eq!(asset.content_hash.len(), 64);
        let json = serde_json::to_string(&asset).unwrap();
        assert!(!json.contains("path"));
    }

    #[test]
    fn capability_profile_distinguishes_supported_unsupported_and_unknown() {
        let image = raw(MediaRepresentationKind::RawImage, "image/png");
        let asset = asset("image/png", 2);
        let mut profile = CapabilityProfile::default();
        assert_eq!(profile.supports(&image, &asset), CapabilitySupport::Unknown);
        profile.image = CapabilitySupport::Supported;
        assert_eq!(
            profile.supports(&image, &asset),
            CapabilitySupport::Supported
        );
        profile.image = CapabilitySupport::Unsupported;
        assert_eq!(
            profile.supports(&image, &asset),
            CapabilitySupport::Unsupported
        );
    }

    #[test]
    fn auto_prefers_raw_and_does_not_send_unsupported_raw() {
        let image = input(
            asset("image/png", 2),
            vec![raw(MediaRepresentationKind::RawImage, "image/png")],
        );
        let mut profile = CapabilityProfile::default();
        profile.image = CapabilitySupport::Supported;
        let plan = build_media_plan(&[image], &profile, MediaInputStrategy::Auto);
        assert_eq!(plan.projections[0].mode, MediaProjectionMode::Raw);

        let image = input(
            asset("image/png", 2),
            vec![
                raw(MediaRepresentationKind::RawImage, "image/png"),
                derived(MediaRepresentationKind::OcrText, "识别结果"),
            ],
        );
        profile.image = CapabilitySupport::Unsupported;
        let plan = build_media_plan(&[image], &profile, MediaInputStrategy::Auto);
        assert_eq!(plan.projections[0].mode, MediaProjectionMode::Derived);
        assert!(
            plan.notices
                .iter()
                .any(|notice| notice.code == MediaPlanNoticeCode::DerivedFallback)
        );
    }

    #[test]
    fn unknown_raw_capability_falls_back_to_derived() {
        let input = input(
            asset("audio/wav", 8),
            vec![
                raw(MediaRepresentationKind::RawAudio, "audio/wav"),
                derived(MediaRepresentationKind::Transcript, "语音内容"),
            ],
        );
        let plan = build_media_plan(
            &[input],
            &CapabilityProfile::default(),
            MediaInputStrategy::Auto,
        );
        assert_eq!(
            plan.projections[0].representation,
            MediaRepresentationKind::Transcript
        );
        assert!(
            plan.notices
                .iter()
                .any(|notice| notice.code == MediaPlanNoticeCode::RawCapabilityUnknown)
        );
    }

    #[test]
    fn extracted_preferred_and_text_only_safe_never_need_raw_capability() {
        let derived_input = input(
            asset("image/png", 2),
            vec![
                raw(MediaRepresentationKind::RawImage, "image/png"),
                derived(MediaRepresentationKind::OcrText, "文字"),
            ],
        );
        let profile = CapabilityProfile::default();
        let preferred = build_media_plan(
            &[derived_input.clone()],
            &profile,
            MediaInputStrategy::ExtractedPreferred,
        );
        assert_eq!(preferred.projections[0].mode, MediaProjectionMode::Derived);
        let safe = build_media_plan(&[derived_input], &profile, MediaInputStrategy::TextOnlySafe);
        assert_eq!(safe.projections[0].mode, MediaProjectionMode::Derived);
        assert!(safe.projections[0].provenance != MediaProvenance::Original);

        let raw_only = input(
            asset("image/png", 2),
            vec![raw(MediaRepresentationKind::RawImage, "image/png")],
        );
        let mut profile = CapabilityProfile::default();
        profile.image = CapabilitySupport::Supported;
        let fallback = build_media_plan(
            &[raw_only],
            &profile,
            MediaInputStrategy::ExtractedPreferred,
        );
        assert_eq!(fallback.projections[0].mode, MediaProjectionMode::Raw);
    }

    #[test]
    fn managed_reference_requires_explicit_tool_or_upload_support() {
        let input = input(
            asset("application/pdf", 100),
            vec![MediaRepresentation::available(
                MediaRepresentationKind::ManagedFileRef,
                MediaProvenance::Original,
                MediaRepresentationPayload::ManagedFileRef {
                    asset_id: "asset-test".into(),
                    filename: Some("report.pdf".into()),
                },
            )],
        );
        let unknown = build_media_plan(
            &[input.clone()],
            &CapabilityProfile::default(),
            MediaInputStrategy::Auto,
        );
        assert!(unknown.is_empty());
        let mut profile = CapabilityProfile::default();
        profile.tools = CapabilitySupport::Supported;
        let plan = build_media_plan(&[input], &profile, MediaInputStrategy::Auto);
        assert_eq!(
            plan.projections[0].mode,
            MediaProjectionMode::ManagedReference
        );
    }

    #[test]
    fn mime_size_and_part_limits_are_enforced() {
        let first = input(
            asset("image/png", 8),
            vec![raw(MediaRepresentationKind::RawImage, "image/png")],
        );
        let second = input(
            asset("image/jpeg", 8),
            vec![raw(MediaRepresentationKind::RawImage, "image/jpeg")],
        );
        let mut profile = CapabilityProfile::default();
        profile.image = CapabilitySupport::Supported;
        profile.accepted_mime_types = vec!["image/png".into()];
        profile.max_input_parts = Some(1);
        profile.max_input_bytes = Some(10);
        let plan = build_media_plan(&[first, second], &profile, MediaInputStrategy::Auto);
        assert_eq!(plan.projections.len(), 1);
        assert!(
            plan.notices
                .iter()
                .any(|notice| notice.code == MediaPlanNoticeCode::RawMimeUnsupported)
        );

        profile.accepted_mime_types.clear();
        let first = input(
            asset("image/png", 8),
            vec![raw(MediaRepresentationKind::RawImage, "image/png")],
        );
        let second = input(
            asset("image/jpeg", 8),
            vec![raw(MediaRepresentationKind::RawImage, "image/jpeg")],
        );
        let plan = build_media_plan(&[first, second], &profile, MediaInputStrategy::Auto);
        assert!(
            plan.notices
                .iter()
                .any(|notice| notice.code == MediaPlanNoticeCode::InputPartLimit)
        );
    }

    #[test]
    fn strategy_parse_defaults_unknown_values_to_auto() {
        assert_eq!(
            MediaInputStrategy::parse("raw_preferred"),
            MediaInputStrategy::RawPreferred
        );
        assert_eq!(MediaInputStrategy::parse("bogus"), MediaInputStrategy::Auto);
        assert_eq!(MediaInputStrategy::Auto.as_str(), "auto");
    }
}
