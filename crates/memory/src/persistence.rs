//! Composition boundary for Memory's SQLite-backed stores.
//!
//! The backing database and repository constructors stay inside this crate.
//! Cross-crate consumers receive only the typed persistence capabilities they
//! need, while `Database` remains available through the explicit test-support
//! feature for raw fixtures.

use std::path::Path;
use std::sync::Arc;

use crate::db::Database;
use crate::repositories::embedding_store::MemoryEmbeddingStore;
use crate::repositories::fact_store::MemoryFactStore;
use crate::repositories::memory_fact_extraction_store::MemoryFactExtractionStore;
use crate::repositories::memory_fact_proposal_store::MemoryFactProposalStore;
use crate::repositories::memory_maintenance_store::MemoryMaintenanceStore;
use crate::repositories::memory_outbox_store::MemoryOutboxStore;
use crate::repositories::memory_recall_store::MemoryRecallStore;
use crate::repositories::schedule_store::ScheduleStore;
use crate::repositories::session_events::SessionStore;
use crate::repositories::tool_run_store::ToolRunStore;

/// Opens the backing database and creates the typed Memory persistence ports.
///
/// This is the only production entry point for constructing database-backed
/// stores outside the Memory crate. Each call to `session_store` creates an
/// independent live-event channel over the same database.
pub struct MemoryPersistence {
    db: Arc<Database>,
}

/// Store capabilities shared with Agent and App composition.
pub struct MemoryStores {
    outbox: MemoryOutboxStore,
    facts: MemoryFactStore,
    fact_extraction: MemoryFactExtractionStore,
    fact_proposals: MemoryFactProposalStore,
    maintenance: MemoryMaintenanceStore,
    recall: MemoryRecallStore,
    embeddings: MemoryEmbeddingStore,
    #[cfg(feature = "test-support")]
    test_database: Option<Arc<Database>>,
}

impl MemoryPersistence {
    /// Open the current database schema and retain its private backing handle.
    pub fn open(path: &Path) -> anyhow::Result<Self> {
        Ok(Self {
            db: Arc::new(Database::open(path)?),
        })
    }

    /// Create an App/Agent session persistence boundary with its own live
    /// event channel over the shared database.
    pub fn session_store(&self) -> SessionStore {
        SessionStore::new(self.db.clone())
    }

    /// Create the store bundle consumed by `MemoryService`.
    pub fn memory_stores(&self) -> MemoryStores {
        MemoryStores::new(self.db.clone())
    }

    /// Create the typed ToolRun persistence port.
    pub fn tool_run_store(&self) -> ToolRunStore {
        ToolRunStore::new(self.db.clone())
    }

    /// Create the typed Schedule plan and occurrence persistence port.
    pub fn schedule_store(&self) -> ScheduleStore {
        ScheduleStore::new(self.db.clone())
    }
}

impl MemoryStores {
    fn new(db: Arc<Database>) -> Self {
        Self {
            outbox: MemoryOutboxStore::new(db.clone()),
            facts: MemoryFactStore::new(db.clone()),
            fact_extraction: MemoryFactExtractionStore::new(db.clone()),
            fact_proposals: MemoryFactProposalStore::new(db.clone()),
            maintenance: MemoryMaintenanceStore::new(db.clone()),
            recall: MemoryRecallStore::new(db.clone()),
            embeddings: MemoryEmbeddingStore::new(db),
            #[cfg(feature = "test-support")]
            test_database: None,
        }
    }

    /// Return the store that commits compaction episodes with extraction markers.
    pub fn outbox_store(&self) -> MemoryOutboxStore {
        self.outbox.clone()
    }

    /// Return the shared fact persistence capability.
    pub fn fact_store(&self) -> MemoryFactStore {
        self.facts.clone()
    }

    /// Return ordinary and summary extraction cursor/transcript persistence.
    pub fn fact_extraction_store(&self) -> MemoryFactExtractionStore {
        self.fact_extraction.clone()
    }

    /// Return the typed queue for pending inferred facts and review actions.
    pub fn fact_proposal_store(&self) -> MemoryFactProposalStore {
        self.fact_proposals.clone()
    }

    /// Return deterministic memory maintenance operations.
    pub fn maintenance_store(&self) -> MemoryMaintenanceStore {
        self.maintenance.clone()
    }

    /// Return typed keyword and vector recall operations.
    pub fn recall_store(&self) -> MemoryRecallStore {
        self.recall.clone()
    }

    /// Return embedding index persistence operations.
    pub fn embedding_store(&self) -> MemoryEmbeddingStore {
        self.embeddings.clone()
    }

    #[cfg(feature = "test-support")]
    pub fn database_handle_for_test(&self) -> Option<Arc<Database>> {
        self.test_database.clone()
    }
}

#[cfg(feature = "test-support")]
impl From<Arc<Database>> for MemoryStores {
    fn from(db: Arc<Database>) -> Self {
        let mut stores = Self::new(db.clone());
        stores.test_database = Some(db);
        stores
    }
}
