use super::{SessionStore, ensure_message_or_step_id, ensure_session_id};
use crate::repositories::kv_store::MAX_MEMORY_SESSION_ID_PAGE_SIZE;
use crate::repositories::sessions::{Session, SessionOrigin};
use haven_common::lifecycle::SessionStatus;
use std::path::PathBuf;
use tokio_util::sync::CancellationToken;

impl SessionStore {
    /// Mark orphaned running sessions as errored through the typed session
    /// persistence boundary. The underlying Database operation retains its
    /// partial-message promotion and cache invalidation semantics.
    pub async fn finalize_orphaned_running_sessions(&self) -> anyhow::Result<usize> {
        self.db
            .run_blocking(|db| db.finalize_orphaned_running_sessions())
            .await
    }

    /// Delete sessions older than the supplied retention window through the
    /// typed session boundary. The repository operation remains responsible
    /// for session-scoped memory cleanup and cache invalidation.
    pub async fn delete_old_sessions(&self, retention_days: u32) -> anyhow::Result<usize> {
        self.db
            .run_blocking(move |db| db.delete_old_sessions(retention_days))
            .await
    }

    /// Return sessions past the supplied retention age so the owning
    /// supervisor can quiesce runtime state before it deletes their rows.
    pub async fn list_old_session_ids(&self, retention_days: u32) -> anyhow::Result<Vec<String>> {
        self.db
            .run_blocking(move |db| db.list_old_session_ids(retention_days))
            .await
    }

    /// Return host-managed attachment paths still referenced by messages.
    /// This is a read-only typed port used by media retention cleanup.
    pub async fn list_managed_attachment_paths(&self) -> anyhow::Result<Vec<PathBuf>> {
        self.db
            .run_blocking(|db| db.list_managed_attachment_paths())
            .await
    }

    /// Persist a session lifecycle status on SQLite's blocking pool.
    ///
    /// The caller owns lifecycle retry and in-memory transition policy; this
    /// port only schedules the existing Database write. Dropping this future
    /// cannot interrupt a write already running on Tokio's blocking pool.
    pub async fn update_session_status(
        &self,
        session_id: &str,
        status: SessionStatus,
    ) -> anyhow::Result<()> {
        ensure_session_id(session_id)?;
        let session_id = session_id.to_owned();
        self.db
            .run_blocking(move |db| db.update_session_status(&session_id, status))
            .await
    }

    /// Persist the last user-visible explanation for a completed, failed, or
    /// paused session run. A new pending/running transition clears this value.
    pub async fn set_session_run_end_reason(
        &self,
        session_id: &str,
        reason: &str,
    ) -> anyhow::Result<()> {
        ensure_session_id(session_id)?;
        let session_id = session_id.to_owned();
        let reason = reason.to_owned();
        self.db
            .run_blocking(move |db| db.set_session_run_end_reason(&session_id, &reason))
            .await
    }

    /// Delete one durable session through the session persistence boundary.
    ///
    /// The existing Database method remains responsible for cascades,
    /// session-scoped cleanup, and cache invalidation. Dropping this future
    /// cannot interrupt a write already running on Tokio's blocking pool.
    pub async fn delete_session(&self, session_id: &str) -> anyhow::Result<()> {
        ensure_session_id(session_id)?;
        let session_id = session_id.to_owned();
        self.db
            .run_blocking(move |db| db.delete_session(&session_id))
            .await
    }

    /// Delete a persisted message through the session persistence boundary.
    ///
    /// The existing Database method remains responsible for message cache
    /// invalidation. This port only schedules that operation on the blocking
    /// pool and returns its result unchanged.
    pub async fn delete_message_by_id(
        &self,
        session_id: &str,
        message_id: &str,
    ) -> anyhow::Result<()> {
        ensure_session_id(session_id)?;
        ensure_message_or_step_id(message_id, "message_id")?;
        let session_id = session_id.to_owned();
        let message_id = message_id.to_owned();
        self.db
            .run_blocking(move |db| db.delete_message_by_id(&session_id, &message_id))
            .await
    }

    /// Atomically clear durable sessions through the session persistence
    /// boundary and return the number of deleted session rows.
    ///
    /// The existing Database method remains responsible for the transaction,
    /// session-scoped cleanup, and cache invalidation. Dropping this future
    /// cannot interrupt a write already running on Tokio's blocking pool.
    pub async fn delete_all_sessions(&self) -> anyhow::Result<usize> {
        self.db
            .clone()
            .run_blocking(|db| db.delete_all_sessions())
            .await
    }

    /// Read all persisted session ids on SQLite's blocking pool.
    pub async fn list_session_ids_cancellable(
        &self,
        cancel: CancellationToken,
    ) -> anyhow::Result<Vec<String>> {
        let ids = self
            .db
            .run_blocking_cancellable(cancel, |db| db.list_session_ids())
            .await?;
        for session_id in &ids {
            ensure_session_id(session_id)?;
        }
        Ok(ids)
    }

    pub async fn session_id_high_water_cancellable(
        &self,
        cancel: CancellationToken,
    ) -> anyhow::Result<Option<String>> {
        let session_id = self
            .db
            .run_blocking_cancellable(cancel, |db| db.session_id_high_water())
            .await?;
        if let Some(session_id) = &session_id {
            ensure_session_id(session_id)?;
        }
        Ok(session_id)
    }

    pub async fn list_session_ids_page_cancellable(
        &self,
        after_session_id: Option<String>,
        high_water_session_id: String,
        cancel: CancellationToken,
    ) -> anyhow::Result<Vec<String>> {
        if let Some(after_session_id) = &after_session_id {
            ensure_session_id(after_session_id)?;
        }
        ensure_session_id(&high_water_session_id)?;
        let ids = self
            .db
            .run_blocking_cancellable(cancel, move |db| {
                db.list_session_ids_page(
                    after_session_id.as_deref(),
                    &high_water_session_id,
                    MAX_MEMORY_SESSION_ID_PAGE_SIZE,
                )
            })
            .await?;
        for session_id in &ids {
            ensure_session_id(session_id)?;
        }
        Ok(ids)
    }

    /// Create a durable session record through the session persistence
    /// boundary. Actor installation, lifecycle gates, and dispatch remain
    /// owned by the Agent layer.
    pub async fn create_session(&self, input_text: &str) -> anyhow::Result<Session> {
        self.create_session_with_origin(input_text, SessionOrigin::User)
            .await
    }

    /// Create a session with typed provenance. Agent-spawn parents are
    /// validated and persisted by the same SessionStore boundary as user
    /// sessions; runtime admission remains owned by Agent.
    pub async fn create_session_with_origin(
        &self,
        input_text: &str,
        origin: SessionOrigin,
    ) -> anyhow::Result<Session> {
        if let SessionOrigin::AgentSpawn { parent_session_id } = &origin {
            ensure_session_id(parent_session_id)?;
        }
        let input_text = input_text.to_owned();
        let session = self
            .db
            .run_blocking(move |db| db.create_session_with_origin(&input_text, origin))
            .await?;
        ensure_session_id(&session.session_id)?;
        Ok(session)
    }

    /// Read direct children from the durable session lineage projection.
    /// Messaging's inbox/profile registry remains the source for live peer
    /// capabilities and mailbox state.
    pub async fn load_session_children(
        &self,
        parent_session_id: &str,
        limit: i64,
        offset: i64,
    ) -> anyhow::Result<Vec<Session>> {
        ensure_session_id(parent_session_id)?;
        let parent_session_id = parent_session_id.to_owned();
        self.db
            .run_blocking(move |db| db.list_session_children(&parent_session_id, limit, offset))
            .await
    }

    /// Pause all running sessions during the synchronous application exit
    /// callback. This delegates to the existing repository operation so its
    /// status transition, update count, and crash recovery behavior stay the
    /// same.
    pub fn pause_running_sessions(&self) -> anyhow::Result<usize> {
        self.db.pause_running_sessions()
    }

    /// Read a persisted session record by id on SQLite's blocking pool.
    /// Missing records remain `Ok(None)` so lifecycle callers can preserve
    /// their existing not-found behavior.
    pub async fn load_session_record(&self, session_id: &str) -> anyhow::Result<Option<Session>> {
        ensure_session_id(session_id)?;
        let session_id = session_id.to_owned();
        self.db
            .run_blocking(move |db| db.get_session(&session_id))
            .await
    }

    /// Read only the title needed by the messaging heartbeat projection.
    /// Keeping this narrow avoids exposing the raw Database to Agent context
    /// assembly merely for a presentation field.
    pub async fn get_session_title(&self, session_id: &str) -> anyhow::Result<Option<String>> {
        ensure_session_id(session_id)?;
        let session_id = session_id.to_owned();
        self.db
            .run_blocking(move |db| Ok(db.get_session(&session_id)?.and_then(|s| s.title)))
            .await
    }

    /// Read the title shown when ending a session, falling back to its input
    /// text when no title has been assigned. Missing sessions remain `None`.
    /// SQLite work runs on the blocking pool; dropping this future cannot
    /// interrupt a query that has already started.
    pub async fn get_session_title_or_input(
        &self,
        session_id: &str,
    ) -> anyhow::Result<Option<String>> {
        ensure_session_id(session_id)?;
        let session_id = session_id.to_owned();
        self.db
            .run_blocking(move |db| {
                Ok(db.get_session(&session_id)?.map(|session| {
                    session
                        .title
                        .filter(|title| !title.is_empty())
                        .unwrap_or(session.input_text)
                }))
            })
            .await
    }

    /// Persist a session title on SQLite's blocking pool.
    ///
    /// This delegates to the existing Database operation so its cache
    /// invalidation and error behavior remain authoritative. Dropping this
    /// future cannot interrupt a write already running on Tokio's blocking
    /// pool.
    pub async fn update_session_title(&self, session_id: &str, title: &str) -> anyhow::Result<()> {
        ensure_session_id(session_id)?;
        let session_id = session_id.to_owned();
        let title = title.to_owned();
        self.db
            .run_blocking(move |db| db.update_session_title(&session_id, &title))
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Database;
    use std::sync::Arc;

    fn store() -> (Arc<Database>, SessionStore, String) {
        let db = Arc::new(Database::open_in_memory().unwrap());
        let session = db.create_session("input").unwrap();
        let store = SessionStore::new(db.clone());
        (db, store, session.session_id)
    }

    #[tokio::test]
    async fn session_store_cleanup_ports_preserve_session_semantics() {
        let db = Arc::new(Database::open_in_memory().unwrap());
        let store = SessionStore::new(db.clone());
        let running = db.create_session("running").unwrap();
        db.update_session_status(&running.session_id, SessionStatus::Running)
            .unwrap();

        assert_eq!(store.finalize_orphaned_running_sessions().await.unwrap(), 1);
        assert_eq!(
            db.get_session(&running.session_id).unwrap().unwrap().status,
            SessionStatus::Error
        );

        let retained = db.create_session("retained before cleanup").unwrap();
        assert_eq!(store.delete_old_sessions(0).await.unwrap(), 2);
        assert_eq!(db.count_sessions().unwrap(), 0);
        assert!(db.get_session(&retained.session_id).unwrap().is_none());
    }

    #[test]
    fn session_store_pauses_running_sessions_and_returns_updated_count() {
        let (db, store, first_running_id) = store();
        db.update_session_status(&first_running_id, SessionStatus::Running)
            .unwrap();

        let second_running = db.create_session("second running").unwrap();
        db.update_session_status(&second_running.session_id, SessionStatus::Running)
            .unwrap();
        let already_paused = db.create_session("already paused").unwrap();
        db.update_session_status(&already_paused.session_id, SessionStatus::Paused)
            .unwrap();
        let still_pending = db.create_session("still pending").unwrap();
        let completed = db.create_session("completed").unwrap();
        db.update_session_status(&completed.session_id, SessionStatus::Completed)
            .unwrap();

        assert_eq!(store.pause_running_sessions().unwrap(), 2);
        assert_eq!(
            db.get_session(&first_running_id).unwrap().unwrap().status,
            SessionStatus::Paused
        );
        assert_eq!(
            db.get_session(&second_running.session_id)
                .unwrap()
                .unwrap()
                .status,
            SessionStatus::Paused
        );
        assert_eq!(
            db.get_session(&already_paused.session_id)
                .unwrap()
                .unwrap()
                .status,
            SessionStatus::Paused
        );
        assert_eq!(
            db.get_session(&still_pending.session_id)
                .unwrap()
                .unwrap()
                .status,
            SessionStatus::Pending
        );
        assert_eq!(
            db.get_session(&completed.session_id)
                .unwrap()
                .unwrap()
                .status,
            SessionStatus::Completed
        );
        assert_eq!(store.pause_running_sessions().unwrap(), 0);
    }

    #[tokio::test]
    async fn session_store_updates_status_on_blocking_pool() {
        let (db, store, session_id) = store();

        store
            .update_session_status(&session_id, SessionStatus::Running)
            .await
            .unwrap();

        assert_eq!(
            db.get_session(&session_id).unwrap().unwrap().status,
            SessionStatus::Running
        );
    }

    #[tokio::test]
    async fn session_store_deletes_message_by_id_on_blocking_pool() {
        let (db, store, session_id) = store();
        let keep = db
            .add_message(
                &session_id,
                haven_common::types::CanonicalRole::User,
                "keep",
                Some(haven_common::types::TranscriptMessageKind::Text),
                None,
            )
            .unwrap();
        let delete = db
            .add_message(
                &session_id,
                haven_common::types::CanonicalRole::User,
                "delete",
                Some(haven_common::types::TranscriptMessageKind::Text),
                None,
            )
            .unwrap();

        store
            .delete_message_by_id(&session_id, &delete.message_id)
            .await
            .unwrap();

        let messages = db.list_session_messages(&session_id).unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].message_id, keep.message_id);
    }

    #[tokio::test]
    async fn session_store_deletes_session_and_preserves_database_cleanup_and_error() {
        let (db, store, session_id) = store();
        db.add_message(
            &session_id,
            haven_common::types::CanonicalRole::User,
            "keep only until delete",
            Some(haven_common::types::TranscriptMessageKind::Text),
            None,
        )
        .unwrap();
        db.list_session_messages(&session_id).unwrap();
        let kv_key = format!("fact_extraction_pending.{session_id}");
        db.set_kv(&kv_key, "1").unwrap();

        store.delete_session(&session_id).await.unwrap();

        assert!(db.get_session(&session_id).unwrap().is_none());
        assert!(db.list_session_messages(&session_id).unwrap().is_empty());
        assert!(db.get_kv(&kv_key).unwrap().is_none());
        let error = store.delete_session(&session_id).await.unwrap_err();
        assert_eq!(
            error.to_string(),
            format!("session '{}' not found in database", session_id)
        );
    }

    #[tokio::test]
    async fn session_store_clears_sessions_and_returns_deleted_row_count() {
        let (db, store, first_session_id) = store();
        let second = db.create_session("second input").unwrap();
        db.add_message(
            &first_session_id,
            haven_common::types::CanonicalRole::User,
            "first message",
            Some(haven_common::types::TranscriptMessageKind::Text),
            None,
        )
        .unwrap();
        db.add_message(
            &second.session_id,
            haven_common::types::CanonicalRole::User,
            "second message",
            Some(haven_common::types::TranscriptMessageKind::Text),
            None,
        )
        .unwrap();
        let kv_key = format!("fact_extraction_pending.{first_session_id}");
        db.set_kv(&kv_key, "1").unwrap();

        assert_eq!(store.delete_all_sessions().await.unwrap(), 2);

        assert_eq!(db.count_sessions().unwrap(), 0);
        assert!(
            db.list_session_messages(&first_session_id)
                .unwrap()
                .is_empty()
        );
        assert!(
            db.list_session_messages(&second.session_id)
                .unwrap()
                .is_empty()
        );
        assert!(db.get_kv(&kv_key).unwrap().is_none());
        assert_eq!(store.delete_all_sessions().await.unwrap(), 0);
    }

    #[tokio::test]
    async fn session_store_loads_session_records_and_preserves_missing_as_none() {
        let (_db, store, session_id) = store();
        assert_eq!(
            store
                .load_session_record(&session_id)
                .await
                .unwrap()
                .unwrap()
                .session_id,
            session_id
        );

        let missing_session_id = haven_common::types::new_id("ses");
        assert!(
            store
                .load_session_record(&missing_session_id)
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn session_store_gets_only_the_session_title_for_context_assembly() {
        let (db, store, session_id) = store();
        db.update_session_title(&session_id, "Context title")
            .unwrap();

        assert_eq!(
            store
                .get_session_title(&session_id)
                .await
                .unwrap()
                .as_deref(),
            Some("Context title")
        );
        let missing_session_id = haven_common::types::new_id("ses");
        assert!(
            store
                .get_session_title(&missing_session_id)
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn session_store_gets_title_or_input_text_for_end_session() {
        let (db, store, session_id) = store();

        assert_eq!(
            store
                .get_session_title_or_input(&session_id)
                .await
                .unwrap()
                .as_deref(),
            Some("input")
        );

        db.update_session_title(&session_id, "Stored title")
            .unwrap();
        assert_eq!(
            store
                .get_session_title_or_input(&session_id)
                .await
                .unwrap()
                .as_deref(),
            Some("Stored title")
        );

        db.update_session_title(&session_id, "").unwrap();
        assert_eq!(
            store
                .get_session_title_or_input(&session_id)
                .await
                .unwrap()
                .as_deref(),
            Some("input")
        );

        let missing_session_id = haven_common::types::new_id("ses");
        assert!(
            store
                .get_session_title_or_input(&missing_session_id)
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn session_store_updates_session_title_and_invalidates_history_cache() {
        let (db, store, session_id) = store();
        assert_eq!(db.list_persisted_sessions(50, 0).unwrap()[0].title, None);

        store
            .update_session_title(&session_id, "Updated through SessionStore")
            .await
            .unwrap();

        assert_eq!(
            store.list_session_history(50, 0).await.unwrap()[0]
                .title
                .as_deref(),
            Some("Updated through SessionStore")
        );
    }

    #[tokio::test]
    async fn session_store_creates_session_without_appending_events() {
        let db = Arc::new(Database::open_in_memory().unwrap());
        let store = SessionStore::new(db.clone());

        let session = store.create_session("created through store").await.unwrap();

        assert!(!session.session_id.is_empty());
        assert_eq!(session.input_text, "created through store");
        assert_eq!(session.status, SessionStatus::Pending);
        assert_eq!(session.origin, SessionOrigin::User);
        assert!(
            store
                .load_session_record(&session.session_id)
                .await
                .unwrap()
                .is_some()
        );
        assert_eq!(store.latest_sequence(&session.session_id).unwrap(), 0);
    }

    #[tokio::test]
    async fn session_store_creates_and_queries_peer_session_provenance() {
        let db = Arc::new(Database::open_in_memory().unwrap());
        let store = SessionStore::new(db);
        let parent = store.create_session("parent").await.unwrap();

        let child = store
            .create_session_with_origin(
                "delegated kickoff",
                SessionOrigin::AgentSpawn {
                    parent_session_id: parent.session_id.clone(),
                },
            )
            .await
            .unwrap();
        let children = store
            .load_session_children(&parent.session_id, 10, 0)
            .await
            .unwrap();

        assert_eq!(children.len(), 1);
        assert_eq!(children[0].session_id, child.session_id);
        assert_eq!(children[0].origin, child.origin);
        assert_eq!(store.latest_sequence(&child.session_id).unwrap(), 0);
    }
}
