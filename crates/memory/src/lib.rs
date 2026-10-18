mod cache;
mod db;
mod embeddings;
mod persistence;
mod recall;
mod repositories;
mod schema;

#[cfg(not(feature = "test-support"))]
pub(crate) use db::Database;
#[cfg(feature = "test-support")]
pub use db::Database;
#[cfg(feature = "test-support")]
pub use embeddings::EmbeddedText;
pub use persistence::{MemoryPersistence, MemoryStores};
pub use recall::{
    MAX_MEMORY_QUERY_CHARS, MAX_RECALL_LIMIT, MemoryEntityKind, MemoryHit, MemoryQuery,
    MemoryRecall, MemoryRecallDiagnostics, MemoryRecallEmptyReason, MemoryRecallMode,
    MemoryRecallSourceStatus, MemoryRecallSuggestion, normalize_memory_query,
};
pub use repositories::embedding_store::{
    MemoryEmbeddingSaveFailure, MemoryEmbeddingSaveReport, MemoryEmbeddingStore,
    MemoryEmbeddingVector, PendingMemoryEmbedding,
};
pub use repositories::fact_store::{
    DEFAULT_FACT_PAGE_SIZE, FactPage, FactPageCursor, MAX_FACT_PAGE_SIZE, MemoryFactStore,
    MemoryFactWrite,
};
pub use repositories::facts::{
    CANONICAL_MERGE_TARGETS, ContradictionCandidate, ContradictionKind, Fact, FactSourceRef,
    UpsertOutcome, fact_effective_confidence, fact_within_demote_age, is_canonical_merge_target,
    is_identity_predicate, is_sensitive_object, is_sensitive_predicate, is_sensitive_text,
    is_visible_fact, normalize_predicate, pick_contradiction_keeper,
};
pub use repositories::kv_store::{
    FactExtractionMarker, FactExtractionMarkerState, SummaryExtractionMarker,
    SummaryExtractionMarkerState,
};
#[cfg(feature = "test-support")]
pub use repositories::kv_store::{MAX_MEMORY_OUTBOX_PAGE_SIZE, MAX_MEMORY_SESSION_ID_PAGE_SIZE};
pub use repositories::memory_fact_extraction_store::{
    FactExtractionProjection, MemoryFactExtractionStore,
};
pub use repositories::memory_maintenance_store::{MemoryMaintenanceStore, PredicateCount};
pub use repositories::memory_outbox_store::MemoryOutboxStore;
pub use repositories::memory_recall_store::MemoryRecallStore;
pub use repositories::messages::{Message, PendingInputDisposition, PendingSessionInput};
#[cfg(feature = "test-support")]
pub use repositories::partials::PartialMessageCheckpoint;
pub use repositories::session_authorization::{
    SessionAuthorizationGrant, StoredSessionAuthorizationGrant,
};
pub use repositories::session_events::{
    ActiveBranchPoint, CONFIRMATION_BATCH_PLANNED_EVENT_TYPE, INTERACTION_CLEARED_EVENT_TYPE,
    INTERACTION_REQUESTED_EVENT_TYPE, INTERACTION_RESOLVED_EVENT_TYPE, MEMORY_TRIGGER_EVENT_TYPE,
    RecoveryPartialKind, RecoveryPersistenceStatus, RollbackProjectionBoundary, RollbackRequest,
    RollbackResult, SessionCommitResult, SessionCommitted, SessionCursor, SessionEvent,
    SessionEventInput, SessionEventPage, SessionForkBranchPoint, SessionForkSnapshot,
    SessionHistoryFilter, SessionHistoryStatusFilter, SessionMessageText, SessionReplayState,
    SessionResumeMedia, SessionResumeProjection, SessionStore, SessionTitleGenerationContext,
    SqliteStorageWriteFailure, TIMELINE_ROLLBACK_EVENT_TYPE, TRANSCRIPT_EVENT_TYPE,
    USAGE_DISCARDED_EVENT_TYPE, USAGE_RECORDED_EVENT_TYPE,
};
pub use repositories::session_steps::{SessionStep, ToolStepOutcome, ToolStepWrite};
pub use repositories::sessions::{Session, SessionOrigin};
pub use repositories::tool_run_completion_outbox::ToolRunCompletionOutboxRow;
pub use repositories::tool_run_store::ToolRunStore;
pub use repositories::tool_runs::{ScheduledToolRunRow, ToolRunDependencyRow, ToolRunRow};
pub use repositories::usage::{LlmUsageRecord, LlmUsageRecordInput, SessionUsage};
pub use repositories::workflow_definitions::{
    CreateWorkflowSchedule, WorkflowDefinitionRow, WorkflowDefinitionStore, WorkflowScheduleRow,
};
