//! Agent-owned ports for reading immutable tool catalog views.

use async_trait::async_trait;
use haven_tools::{ToolCatalogSnapshot, ToolsManager};
use std::sync::Arc;

/// Reads the immutable tool catalog view for one session.
#[async_trait]
pub(crate) trait ToolCatalogPort: Send + Sync {
    async fn catalog_snapshot(&self, session_id: &str) -> Arc<ToolCatalogSnapshot>;
}

/// Production adapter that delegates catalog snapshot creation to tools.
pub(crate) struct ToolsManagerToolCatalogAdapter {
    tools: Arc<ToolsManager>,
}

impl ToolsManagerToolCatalogAdapter {
    pub(crate) fn new(tools: Arc<ToolsManager>) -> Self {
        Self { tools }
    }
}

#[async_trait]
impl ToolCatalogPort for ToolsManagerToolCatalogAdapter {
    async fn catalog_snapshot(&self, session_id: &str) -> Arc<ToolCatalogSnapshot> {
        Arc::new(self.tools.tool_catalog_snapshot(session_id).await)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::react::ReActEngine;
    use crate::session::SessionSupervisor;
    use haven_common::config::{ContextLimitsConfig, RouterConfig};
    use haven_llm::LlmRouter;
    use haven_memory::Database;
    use std::sync::Mutex;

    struct RecordingToolCatalogPort {
        session_ids: Arc<Mutex<Vec<String>>>,
        snapshot: Arc<ToolCatalogSnapshot>,
    }

    #[async_trait]
    impl ToolCatalogPort for RecordingToolCatalogPort {
        async fn catalog_snapshot(&self, session_id: &str) -> Arc<ToolCatalogSnapshot> {
            self.session_ids
                .lock()
                .unwrap()
                .push(session_id.to_string());
            Arc::clone(&self.snapshot)
        }
    }

    #[tokio::test]
    async fn engine_forwards_session_id_and_keeps_snapshot_arc() {
        let directory = tempfile::tempdir().expect("temporary database directory");
        let db = Arc::new(
            Database::open(&directory.path().join("tool-catalog-port.db"))
                .expect("temporary database"),
        );
        let tools = Arc::new(ToolsManager::new());
        let snapshot = Arc::new(tools.tool_catalog_snapshot("ses-seed").await);
        let executor = Arc::new(SessionSupervisor::new(db.clone(), tools, 1));
        let session_ids = Arc::new(Mutex::new(Vec::new()));
        let tool_catalog: Arc<dyn ToolCatalogPort> = Arc::new(RecordingToolCatalogPort {
            session_ids: Arc::clone(&session_ids),
            snapshot: Arc::clone(&snapshot),
        });
        let engine = ReActEngine::new(
            Arc::new(LlmRouter::new(RouterConfig::default())),
            Arc::clone(&tool_catalog),
            executor,
            db,
            1,
            ContextLimitsConfig::default(),
        );
        assert!(Arc::ptr_eq(&engine.tool_catalog, &tool_catalog));

        let requested_session_id = " ses-session-id-with-padding ";
        let returned = engine
            .build_tool_catalog_for_session(requested_session_id)
            .await;

        assert_eq!(
            *session_ids.lock().unwrap(),
            vec![requested_session_id.to_string()]
        );
        assert!(Arc::ptr_eq(&returned, &snapshot));
    }
}
