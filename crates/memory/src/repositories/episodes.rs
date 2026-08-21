use crate::db::Database;

impl Database {
    /// Persist a compaction summary as an episode. Returns the new row id.
    /// Called from the react loop at every compaction (proactive or forced).
    /// The row becomes part of the `episode` memory domain: the embedding
    /// missing-index scan and the keyword search query `memory_episodes`
    /// directly (see embeddings.rs), so no read API is needed here.
    ///
    /// Episodes live in the same id space as messages (`msg-{uuid32}`): the
    /// `episode` memory domain covers user messages and these summaries alike,
    /// and a single shared prefix keeps `entity_id` values unambiguous without
    /// a separate `epi-` prefix.
    pub fn add_episode(&self, session_id: &str, summary: &str) -> anyhow::Result<String> {
        let id = haven_common::types::new_id("msg");
        self.add_episode_with_id(session_id, summary, &id)?;
        Ok(id)
    }

    /// Persist a compaction summary under a caller-minted `msg-*` id so the
    /// canonical summary bubble and `memory_episodes` row share one identity
    /// (L1). `id` must already be a `msg-*` value from `new_id("msg")`.
    pub fn add_episode_with_id(
        &self,
        session_id: &str,
        summary: &str,
        id: &str,
    ) -> anyhow::Result<()> {
        self.add_episode_structured(session_id, summary, id, &[], &[])
    }

    /// Like [`Self::add_episode_with_id`], with optional topic/entity tags
    /// (P2-10 / L6). Stored as JSON string arrays for FTS + future filters.
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
            "INSERT INTO memory_episodes (id, session_id, summary, topics, entities, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
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
        let session = db.create_session("t1", "").unwrap();
        let id = db.add_episode(&session.id, "a compaction summary").unwrap();
        assert!(id.starts_with("msg-"));
        let (summary, session_id): (String, String) = db
            .conn()
            .query_row(
                "SELECT summary, session_id FROM memory_episodes WHERE id = ?1",
                rusqlite::params![id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(summary, "a compaction summary");
        assert_eq!(session_id, session.id);
    }

    #[test]
    fn add_episode_with_id_reuses_caller_id() {
        let db = Database::open_in_memory().unwrap();
        let session = db.create_session("t1", "").unwrap();
        let id = haven_common::types::new_id("msg");
        db.add_episode_with_id(&session.id, "shared id summary", &id)
            .unwrap();
        let got: String = db
            .conn()
            .query_row(
                "SELECT id FROM memory_episodes WHERE summary = ?1",
                rusqlite::params!["shared id summary"],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(got, id);
    }

    #[test]
    fn add_episode_structured_stores_topics_entities() {
        let db = Database::open_in_memory().unwrap();
        let session = db.create_session("t1", "").unwrap();
        let id = haven_common::types::new_id("msg");
        db.add_episode_structured(
            &session.id,
            "summary",
            &id,
            &["theme", "ui"],
            &["Alice"],
        )
        .unwrap();
        let (topics, entities): (String, String) = db
            .conn()
            .query_row(
                "SELECT topics, entities FROM memory_episodes WHERE id = ?1",
                rusqlite::params![id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert!(topics.contains("theme"));
        assert!(entities.contains("Alice"));
    }
}
