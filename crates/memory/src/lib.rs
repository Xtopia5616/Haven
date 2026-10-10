mod cache;
mod db;
mod embeddings;
mod recall;
mod repositories;
mod schema;

pub use cache::CacheGeneration;
pub use db::Database;
pub use embeddings::EmbeddedText;
pub use recall::{
    MAX_MEMORY_QUERY_CHARS, MAX_RECALL_LIMIT, MemoryEntityKind, MemoryHit, MemoryQuery,
    MemoryRecall, MemoryRecallDiagnostics, MemoryRecallEmptyReason, MemoryRecallMode,
    MemoryRecallSourceStatus, MemoryRecallSuggestion, normalize_memory_query,
};
pub use repositories::embedding_store::{
    MemoryEmbeddingSaveFailure, MemoryEmbeddingSaveReport, MemoryEmbeddingStore,
    MemoryEmbeddingVector, PendingMemoryEmbedding,
};
pub use repositories::fact_store::{MemoryFactStore, MemoryFactWrite};
pub use repositories::facts::{
    CANONICAL_MERGE_TARGETS, CONTRADICTION_DEMOTE_MAX_AGE_DAYS, CONTRADICTION_LIVE_FLOOR,
    ContradictionCandidate, ContradictionKind, Fact, FactSourceRef, UpsertOutcome,
    fact_effective_confidence, fact_within_demote_age, filter_visible_facts,
    is_canonical_merge_target, is_identity_predicate, is_sensitive_object, is_sensitive_predicate,
    is_sensitive_text, is_single_valued_predicate, is_visible_fact, is_volatile_predicate,
    normalize_predicate, pick_contradiction_keeper, polarity_opposite,
};
pub use repositories::kv_store::{
    FactExtractionMarker, FactExtractionMarkerState, MAX_MEMORY_OUTBOX_PAGE_SIZE,
    MAX_MEMORY_SESSION_ID_PAGE_SIZE, SummaryExtractionMarker, SummaryExtractionMarkerState,
};
pub use repositories::memory_fact_extraction_store::{
    FactExtractionTranscript, MemoryFactExtractionStore,
};
pub use repositories::memory_maintenance_store::{MemoryMaintenanceStore, PredicateCount};
pub use repositories::memory_recall_store::MemoryRecallStore;
pub use repositories::memory_store::MemoryStore;
pub use repositories::messages::{Message, PendingInputDisposition, PendingSessionInput};
pub use repositories::partials::PartialMessageCheckpoint;
pub use repositories::scheduled_tool_runs::{
    ScheduledToolRunRow, ToolRunDependencyRow, ToolRunRow,
};
pub use repositories::session_authorization::{
    SessionAuthorizationGrant, StoredSessionAuthorizationGrant,
};
pub use repositories::session_events::{
    ActiveBranchPoint, BRANCH_POINT_EVENT_TYPE, CONFIRMATION_BATCH_PLANNED_EVENT_TYPE,
    CURRENT_EVENT_VERSION, INTERACTION_CLEARED_EVENT_TYPE, INTERACTION_REQUESTED_EVENT_TYPE,
    INTERACTION_RESOLVED_EVENT_TYPE, MAX_SESSION_EVENT_REPLAY_PAGE_SIZE,
    MAX_TRANSCRIPT_BATCH_EVENTS, MAX_TRANSCRIPT_BATCH_PROJECTION_ROWS, MEMORY_TRIGGER_EVENT_TYPE,
    ProjectionCutoff, RECOVERY_PERSISTENCE_EVENT_TYPE, RecoveryPartialKind,
    RecoveryPersistenceStatus, RollbackProjectionBoundary, RollbackRequest, RollbackResult,
    SessionCommitResult, SessionCommitted, SessionCommittedEvent, SessionCursor, SessionEvent,
    SessionEventInput, SessionEventPage, SessionEventSubscription, SessionHistoryFilter,
    SessionHistoryStatusFilter, SessionMessageText, SessionProjectionIntent, SessionReplayState,
    SessionResumeMedia, SessionResumeProjection, SessionStore, SessionTitleGenerationContext,
    SqliteStorageWriteFailure, TIMELINE_ROLLBACK_EVENT_TYPE, TRANSCRIPT_EVENT_TYPE,
    USAGE_DISCARDED_EVENT_TYPE, USAGE_RECORDED_EVENT_TYPE,
};
pub use repositories::session_steps::{SessionStep, ToolStepOutcome, ToolStepWrite};
pub use repositories::sessions::{Session, SessionOrigin};
pub use repositories::tool_run_completion_outbox::ToolRunCompletionOutboxRow;
pub use repositories::tool_run_store::ToolRunStore;
pub use repositories::usage::{LlmUsageRecord, LlmUsageRecordInput, SessionUsage};
