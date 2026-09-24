use crate::db::Database;

impl Database {
    /// Persist a compaction summary as a memory item (`episode_summary`).
    /// Returns the new row id.
    ///
    /// Items live in the same id space as messages (`msg-{uuid32}`): the
    /// `episode` embedding domain covers these summaries, and a single shared
    /// prefix keeps `entity_id` values unambiguous without a separate prefix.
    pub fn add_episode(&self, session_id: &str, summary: &str) -> anyhow::Result<String> {
        let id = haven_common::types::new_id("msg");
        self.add_episode_with_id(session_id, summary, &id)?;
        Ok(id)
    }

    /// Persist a compaction summary under a caller-minted `msg-*` id so the
    /// canonical summary bubble and `memory_items` row share one identity.
    /// `id` must already be a `msg-*` value from `new_id("msg")`.
    pub fn add_episode_with_id(
        &self,
        session_id: &str,
        summary: &str,
        id: &str,
    ) -> anyhow::Result<()> {
        self.add_episode_structured(session_id, summary, id, &[], &[])
    }

    /// Persist a compaction episode and, when requested, its summary-fact
    /// extraction marker in one transaction. Replaying the same compaction is
    /// idempotent: an existing row must match the original session/content,
    /// and the pending marker is recreated if an older process stopped before
    /// acknowledging it.
    pub fn add_episode_with_pending_extraction(
        &self,
        session_id: &str,
        summary: &str,
        id: &str,
        enqueue_extraction: bool,
    ) -> anyhow::Result<()> {
        let now = chrono::Utc::now().to_rfc3339();
        let topics_json = "[]";
        let entities_json = "[]";
        let marker_key = format!("fact_extraction_episode_pending.{session_id}.{id}");
        let conn = self.conn();
        conn.execute_batch("BEGIN IMMEDIATE")?;
        let result = (|| -> anyhow::Result<()> {
            conn.execute(
                "INSERT INTO memory_items
                    (id, session_id, kind, content, topics, entities, created_at)
                 VALUES (?1, ?2, 'episode_summary', ?3, ?4, ?5, ?6)
                 ON CONFLICT(id) DO NOTHING",
                rusqlite::params![
                    id,
                    session_id,
                    summary,
                    topics_json,
                    entities_json,
                    now.clone()
                ],
            )?;
            let existing: (String, String, String) = conn.query_row(
                "SELECT session_id, kind, content FROM memory_items WHERE id = ?1",
                rusqlite::params![id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )?;
            anyhow::ensure!(
                existing.0 == session_id
                    && existing.1 == "episode_summary"
                    && existing.2 == summary,
                "episode '{}' already exists with different content",
                id
            );
            if enqueue_extraction {
                conn.execute(
                    "INSERT INTO kv_store (key, value, updated_at)
                     VALUES (?1, ?2, ?3)
                     ON CONFLICT(key) DO UPDATE SET value = excluded.value,
                         updated_at = excluded.updated_at",
                    rusqlite::params![marker_key, session_id, now],
                )?;
            }
            Ok(())
        })();
        match result {
            Ok(()) => {
                conn.execute_batch("COMMIT")?;
                self.cache_invalidate_embeddings(crate::embeddings::entity_kind::EPISODE);
                Ok(())
            }
            Err(error) => {
                let _ = conn.execute_batch("ROLLBACK");
                Err(error)
            }
        }
    }

    /// Like [`Self::add_episode_with_id`], with optional topic/entity tags.
    /// Stored as JSON string arrays for FTS + future filters.
    pub fn add_episode_structured(
        &self,
        session_id: &str,
        summary: &str,
        id: &str,
        topics: &[&str],
        entities: &[&str],
    ) -> anyhow::Result<()> {
        let now = chrono::Utc::now().to_rfc3339();
        let topics_json = serde_json::to_string(topics).unwrap_or_else(|_| "[]".into());
        let entities_json = serde_json::to_string(entities).unwrap_or_else(|_| "[]".into());
        let conn = self.conn();
        conn.execute(
            "INSERT INTO memory_items (id, session_id, kind, content, topics, entities, created_at)
             VALUES (?1, ?2, 'episode_summary', ?3, ?4, ?5, ?6)",
            rusqlite::params![id, session_id, summary, topics_json, entities_json, now],
        )?;
        self.cache_invalidate_embeddings(crate::embeddings::entity_kind::EPISODE);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::Database;

    #[test]
    fn add_episode_persists_row() {
        let db = Database::open_in_memory().unwrap();
        let session = db.create_session("t1").unwrap();
        let id = db.add_episode(&session.id, "a compaction summary").unwrap();
        assert!(id.starts_with("msg-"));
        let (content, session_id, kind): (String, String, String) = db
            .conn()
            .query_row(
                "SELECT content, session_id, kind FROM memory_items WHERE id = ?1",
                rusqlite::params![id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(content, "a compaction summary");
        assert_eq!(session_id, session.id);
        assert_eq!(kind, "episode_summary");
    }

    #[test]
    fn add_episode_with_id_reuses_caller_id() {
        let db = Database::open_in_memory().unwrap();
        let session = db.create_session("t1").unwrap();
        let id = haven_common::types::new_id("msg");
        db.add_episode_with_id(&session.id, "shared id summary", &id)
            .unwrap();
        let got: String = db
            .conn()
            .query_row(
                "SELECT id FROM memory_items WHERE content = ?1",
                rusqlite::params!["shared id summary"],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(got, id);
    }

    #[test]
    fn add_episode_structured_stores_topics_entities() {
        let db = Database::open_in_memory().unwrap();
        let session = db.create_session("t1").unwrap();
        let id = haven_common::types::new_id("msg");
        db.add_episode_structured(&session.id, "summary", &id, &["theme", "ui"], &["Alice"])
            .unwrap();
        let (topics, entities): (String, String) = db
            .conn()
            .query_row(
                "SELECT topics, entities FROM memory_items WHERE id = ?1",
                rusqlite::params![id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert!(topics.contains("theme"));
        assert!(entities.contains("Alice"));
    }

    #[test]
    fn add_episode_with_pending_extraction_is_idempotent_and_recreates_marker() {
        let db = Database::open_in_memory().unwrap();
        let session = db.create_session("t1").unwrap();
        let id = haven_common::types::new_id("msg");
        let summary = "A durable compaction summary with enough context.";

        db.add_episode_with_pending_extraction(&session.id, summary, &id, true)
            .unwrap();
        db.add_episode_with_pending_extraction(&session.id, summary, &id, true)
            .unwrap();
        assert_eq!(
            db.pending_summary_extractions().unwrap(),
            vec![(session.id.clone(), id.clone())]
        );

        db.clear_summary_extraction(&session.id, &id).unwrap();
        assert!(db.pending_summary_extractions().unwrap().is_empty());
        db.add_episode_with_pending_extraction(&session.id, summary, &id, true)
            .unwrap();
        assert_eq!(
            db.pending_summary_extractions().unwrap(),
            vec![(session.id, id)]
        );
    }
}
