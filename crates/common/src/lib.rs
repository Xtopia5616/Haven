pub mod config;
pub mod encoding;
pub mod error;
pub mod hooks;
pub mod media;
pub mod media_detection;
pub mod prompts;
pub mod text;
pub mod tools;
pub mod types;
pub mod workspace;

pub use config::{
    AppConfig, ConfigLoader, LogConfig, LogLevel, McpDiscoveryConfig, McpServerConfig, Settings,
    SkillsExecConfig, default_work_dir,
};
pub use types::McpTransportType;
pub use workspace::discover_workspace_root;

pub use media::{
    CapabilityProfile, CapabilitySupport, MediaAsset, MediaAssetLifecycle, MediaAssetSource,
    MediaDerivation, MediaInput, MediaInputStrategy, MediaModality, MediaPlan, MediaPlanNotice,
    MediaPlanNoticeCode, MediaProjection, MediaProjectionMode, MediaProvenance, MediaReference,
    MediaRepresentation, MediaRepresentationAvailability, MediaRepresentationCost,
    MediaRepresentationKind, MediaRepresentationPayload, MediaResult, build_media_plan,
    message_attachment_to_media_input,
};
pub use media_detection::{
    MediaProbe, MediaType, detect_media_type, detect_media_type_with_filename, detect_modality,
    extension_for_media_type, media_type_from_extension, media_type_from_mime, probe_media,
    probe_media_with_hint,
};

pub use tools::{ToolCatalogGroup, ToolDef, ToolPrompt, ToolRetrySafety};

pub use types::{
    CanonicalMessage, CanonicalRole, CanonicalToolCall, ContentPart, FollowUp, InjectSource,
    MessageAttachment, PEER_KICKOFF_PREFIX,
};
