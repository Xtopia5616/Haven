mod cache;
pub mod db;
pub mod embeddings;
pub mod recall;
pub mod repositories;
pub mod schema;

pub use cache::CacheGeneration;
pub use db::Database;
pub use recall::{
    MemoryEntityKind, MemoryHit, MemoryQuery, MemoryRecall, MemoryRecallDiagnostics,
    MemoryRecallEmptyReason, MemoryRecallMode, MemoryRecallSourceStatus, MemoryRecallSuggestion,
    MemoryRetriever,
};
pub use repositories::embedding_store::{
    MemoryEmbeddingSaveFailure, MemoryEmbeddingSaveReport, MemoryEmbeddingStore,
    MemoryEmbeddingVector, PendingMemoryEmbedding,
};
pub use repositories::fact_store::{MemoryFactStore, MemoryFactWrite};
pub use repositories::kv_store::{MAX_MEMORY_OUTBOX_PAGE_SIZE, MAX_MEMORY_SESSION_ID_PAGE_SIZE};
pub use repositories::memory_fact_extraction_store::{
    FactExtractionTranscript, MemoryFactExtractionStore,
};
pub use repositories::memory_maintenance_store::{MemoryMaintenanceStore, PredicateCount};
pub use repositories::memory_recall_store::MemoryRecallStore;
pub use repositories::memory_store::MemoryStore;
pub use repositories::messages::{PendingInputDisposition, PendingSessionInput};
pub use repositories::scheduled_tool_runs::{
    ScheduledToolRunRow, ToolRunDependencyRow, ToolRunRow,
};
pub use repositories::session_authorization::{
    SessionAuthorizationGrant, StoredSessionAuthorizationGrant,
};
pub use repositories::session_events::{
    BRANCH_POINT_EVENT_TYPE, CURRENT_EVENT_VERSION, INTERACTION_CLEARED_EVENT_TYPE,
    INTERACTION_REQUESTED_EVENT_TYPE, INTERACTION_RESOLVED_EVENT_TYPE,
    MAX_SESSION_EVENT_REPLAY_PAGE_SIZE, MAX_TRANSCRIPT_BATCH_EVENTS,
    MAX_TRANSCRIPT_BATCH_PROJECTION_ROWS, MEMORY_TRIGGER_EVENT_TYPE, ProjectionCutoff,
    RECOVERY_PERSISTENCE_EVENT_TYPE, RecoveryPersistenceStatus, RollbackProjectionBoundary,
    RollbackRequest, RollbackResult, SessionCommitResult, SessionCommitted, SessionCommittedEvent,
    SessionCursor, SessionEvent, SessionEventInput, SessionEventPage, SessionEventSubscription,
    SessionHistoryFilter, SessionMessageText, SessionProjectionIntent, SessionReplayState,
    SessionResumeMedia, SessionResumeProjection, SessionStore, SessionTitleGenerationContext,
    StoredBranchPoint, TIMELINE_ROLLBACK_EVENT_TYPE, TRANSCRIPT_EVENT_TYPE,
    USAGE_DISCARDED_EVENT_TYPE, USAGE_RECORDED_EVENT_TYPE,
};
pub use repositories::sessions::{Session, SessionOrigin};
pub use repositories::tool_run_completion_outbox::ToolRunCompletionOutboxRow;
pub use repositories::tool_run_store::ToolRunStore;
pub use repositories::usage::LlmUsageRecordInput;
