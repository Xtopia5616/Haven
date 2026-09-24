mod cache;
pub mod db;
pub mod embeddings;
pub mod recall;
pub mod repositories;
pub mod schema;

pub use cache::CacheGeneration;
pub use db::Database;
pub use recall::{
    MemoryHit, MemoryKind, MemoryQuery, MemoryRecall, MemoryRecallDiagnostics,
    MemoryRecallEmptyReason, MemoryRecallMode, MemoryRecallSourceStatus, MemoryRecallSuggestion,
    MemoryRetriever,
};
pub use repositories::action_completion_outbox::ActionCompletionOutboxRow;
pub use repositories::action_store::ActionStore;
pub use repositories::embedding_store::{
    MemoryEmbeddingEntity, MemoryEmbeddingSaveFailure, MemoryEmbeddingSaveReport,
    MemoryEmbeddingStore, MemoryEmbeddingVector, PendingMemoryEmbedding,
};
pub use repositories::fact_store::MemoryFactStore;
pub use repositories::memory_fact_extraction_store::{
    FactExtractionTranscript, MemoryFactExtractionStore,
};
pub use repositories::memory_recall_store::MemoryRecallStore;
pub use repositories::memory_store::MemoryStore;
pub use repositories::scheduled_actions::{ActionRow, ScheduledActionRow};
pub use repositories::session_events::{
    BRANCH_POINT_EVENT_TYPE, CURRENT_EVENT_VERSION, INTERACTION_CLEARED_EVENT_TYPE,
    INTERACTION_REQUESTED_EVENT_TYPE, INTERACTION_RESOLVED_EVENT_TYPE,
    MAX_SESSION_EVENT_REPLAY_PAGE_SIZE, MAX_TRANSCRIPT_BATCH_EVENTS,
    MAX_TRANSCRIPT_BATCH_PROJECTION_ROWS, MEMORY_TRIGGER_EVENT_TYPE, ProjectionCutoff,
    RECOVERY_PERSISTENCE_EVENT_TYPE, RecoveryPersistenceStatus, RollbackProjectionBoundary,
    RollbackRequest, RollbackResult, SessionCursor, SessionEvent, SessionEventInput,
    SessionEventPage, SessionEventStore, SessionEventSubscription, SessionHistoryFilter,
    SessionMessageText, SessionReplayState, SessionResumeMedia, SessionResumeProjection,
    SessionStore, SessionTitleGenerationContext, StoredBranchPoint, TIMELINE_ROLLBACK_EVENT_TYPE,
    TRANSCRIPT_EVENT_TYPE, TranscriptActionStepProjection, TranscriptBatch, TranscriptBatchResult,
    TranscriptMessageProjection, TranscriptThoughtStepProjection, USAGE_DISCARDED_EVENT_TYPE,
    USAGE_RECORDED_EVENT_TYPE,
};
pub use repositories::usage::LlmCallUsageInput;
