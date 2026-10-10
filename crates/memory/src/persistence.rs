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
use crate::repositories::memory_maintenance_store::MemoryMaintenanceStore;
use crate::repositories::memory_recall_store::MemoryRecallStore;
use crate::repositories::memory_store::MemoryStore;
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
    pub memory: MemoryStore,
    pub facts: MemoryFactStore,
    pub fact_extraction: MemoryFactExtractionStore,
    pub maintenance: MemoryMaintenanceStore,
    pub recall: MemoryRecallStore,
    pub embeddings: MemoryEmbeddingStore,
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

    /// Create the typed fact capability shared by App commands and Agent.
    pub fn memory_fact_store(&self) -> MemoryFactStore {
        MemoryFactStore::new(self.db.clone())
    }

    /// Create the store bundle consumed by `MemoryService`.
    pub fn memory_stores(&self) -> MemoryStores {
        MemoryStores::new(self.db.clone())
    }

    /// Create the typed ToolRun persistence port.
    pub fn tool_run_store(&self) -> ToolRunStore {
        ToolRunStore::new(self.db.clone())
    }
}

impl MemoryStores {
    fn new(db: Arc<Database>) -> Self {
        Self {
            memory: MemoryStore::new(db.clone()),
            facts: MemoryFactStore::new(db.clone()),
            fact_extraction: MemoryFactExtractionStore::new(db.clone()),
            maintenance: MemoryMaintenanceStore::new(db.clone()),
            recall: MemoryRecallStore::new(db.clone()),
            embeddings: MemoryEmbeddingStore::new(db),
            #[cfg(feature = "test-support")]
            test_database: None,
        }
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
