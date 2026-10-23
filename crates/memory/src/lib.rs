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
    MemoryRecall, MemoryRecallDiagnostics, MemoryRecallEmptyReason, MemoryRecallKind,
    MemoryRecallMode, MemoryRecallSourceStatus, MemoryRecallSuggestion, normalize_memory_query,
};
pub use repositories::embedding_store::{
    MemoryEmbeddingPersistFailure, MemoryEmbeddingPersistReport, MemoryEmbeddingStore,
    MemoryEmbeddingVector, PendingMemoryEmbedding,
};
pub use repositories::fact_store::{
    DEFAULT_FACT_PAGE_SIZE, FactHistorySnapshot, FactPage, FactPageCursor, FactValiditySnapshot,
    MAX_FACT_HISTORY_RESULTS, MAX_FACT_PAGE_SIZE, MemoryFactStore, MemoryFactWrite,
};
pub use repositories::facts::{
    CANONICAL_MERGE_TARGETS, ContradictionCandidate, ContradictionKind, Fact, FactSourceRef,
    FactValidity, MemoryFactKind, UpsertOutcome, fact_effective_confidence, fact_within_demote_age,
    is_canonical_merge_target, is_identity_predicate, is_sensitive_object, is_sensitive_predicate,
    is_sensitive_text, is_visible_fact, normalize_fact_validity, normalize_predicate,
    pick_contradiction_keeper,
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
pub use repositories::memory_fact_proposal_store::{
    FactEvidenceStatus, FactProposal, FactProposalCommitOutcome, FactProposalCursor,
    FactProposalDecision, FactProposalPage, FactProposalReviewOutcome, MAX_FACT_PROPOSAL_PAGE_SIZE,
    MAX_PENDING_FACT_PROPOSALS, MemoryFactProposalStore, MemoryFactProposalWrite,
    VERIFIED_FACT_CONFIDENCE_FLOOR,
};
pub use repositories::memory_maintenance_store::{MemoryMaintenanceStore, PredicateCount};
pub use repositories::memory_outbox_store::MemoryOutboxStore;
pub use repositories::memory_recall_store::MemoryRecallStore;
pub use repositories::messages::{Message, PendingInputDisposition, PendingSessionInput};
#[cfg(feature = "test-support")]
pub use repositories::partials::PartialMessageCheckpoint;
pub use repositories::schedule_store::{
    SchedulePlanInput, SchedulePlanRow, ScheduleRunFired, ScheduleRunRow, ScheduleStore,
    ScheduleType, WatchedToolRunResult,
};
pub use repositories::session_authorization::{
    SessionAuthorizationGrant, StoredSessionAuthorizationGrant,
};
pub use repositories::session_events::{
    ActiveBranchPoint, CONFIRMATION_BATCH_PLANNED_EVENT_TYPE, INTERACTION_CLEARED_EVENT_TYPE,
    INTERACTION_REQUESTED_EVENT_TYPE, INTERACTION_RESOLVED_EVENT_TYPE,
    MAX_SESSION_ASSET_SEARCH_RESULTS, MAX_SESSION_HISTORY_QUERY_BYTES,
    MAX_SESSION_HISTORY_SEARCH_RESULTS, MAX_SESSION_LINKED_HISTORY_CURSOR_TIMESTAMP_BYTES,
    MAX_SESSION_LINKED_HISTORY_SEARCH_SESSIONS, MEMORY_TRIGGER_EVENT_TYPE, RecoveryPartialKind,
    RecoveryPersistenceStatus, RollbackProjectionBoundary, RollbackRequest, RollbackResult,
    SESSION_EXECUTION_MODE_EVENT_TYPE, SESSION_EXECUTION_WORKSPACE_EVENT_TYPE, SessionCommitResult,
    SessionCommitted, SessionCursor, SessionEvent, SessionEventInput, SessionEventPage,
    SessionForkBranchPoint, SessionForkSnapshot, SessionHistoryFilter, SessionHistoryStatusFilter,
    SessionLinkedTranscriptHistoryCursor, SessionLinkedTranscriptHistoryMatch,
    SessionLinkedTranscriptHistorySearch, SessionMessageText, SessionReplayState,
    SessionResumeMedia, SessionResumeProjection, SessionStore, SessionTitleGenerationContext,
    SessionTranscriptAsset, SessionTranscriptAssetMatch, SessionTranscriptAssetSearch,
    SessionTranscriptHistoryMatch, SessionTranscriptHistoryRecord, SessionTranscriptHistorySearch,
    SqliteStorageWriteFailure, TIMELINE_ROLLBACK_EVENT_TYPE, TRANSCRIPT_EVENT_TYPE,
    USAGE_DISCARDED_EVENT_TYPE, USAGE_RECORDED_EVENT_TYPE,
};
pub use repositories::session_steps::{SessionStep, ToolStepOutcome, ToolStepWrite};
pub use repositories::sessions::{
    Session, SessionCreation, SessionCreationMethod, SessionCreationSource, SessionCreator,
    SessionLink,
};
pub use repositories::tool_run_completion_outbox::ToolRunCompletionOutboxRow;
pub use repositories::tool_run_store::ToolRunStore;
pub use repositories::tool_runs::ToolRunRow;
pub use repositories::usage::{LlmUsageRecord, LlmUsageRecordInput, SessionUsage};
