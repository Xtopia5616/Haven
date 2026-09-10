pub mod config;
pub mod encoding;
pub mod error;
pub mod hooks;
pub mod media;
pub mod prompts;
pub mod text;
pub mod tools;
pub mod types;

pub use config::{
    AppConfig, ConfigLoader, LogConfig, LogLevel, McpDiscoveryConfig, McpServerConfig, Settings,
    SkillsExecConfig, default_work_dir,
};
pub use types::McpTransportType;

pub use media::{
    CapabilityProfile, CapabilitySupport, MediaAsset, MediaAssetLifecycle, MediaAssetSource,
    MediaDerivation, MediaInput, MediaInputStrategy, MediaModality, MediaPlan, MediaPlanNotice,
    MediaPlanNoticeCode, MediaProjection, MediaProjectionMode, MediaProvenance,
    MediaRepresentation, MediaRepresentationAvailability, MediaRepresentationCost,
    MediaRepresentationKind, MediaRepresentationPayload, build_media_plan,
    legacy_attachment_to_media_input,
};

pub use tools::ToolDef;

pub use types::{
    CanonicalMessage, CanonicalRole, CanonicalToolCall, ContentPart, InjectSource,
    MessageAttachment, PEER_KICKOFF_PREFIX, Supplement,
};
