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
        let now = chrono::Utc::now().to_rfc3339();
        let conn = self.conn();
        conn.execute(
            "INSERT INTO memory_episodes (id, session_id, summary, created_at) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![id, session_id, summary, now],
        )?;
        self.cache_invalidate_embeddings(crate::embeddings::entity_kind::EPISODE);
        Ok(id)
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
}
