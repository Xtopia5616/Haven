use crate::db::Database;

/// A stored text embedding for one memory entity (fact or episode).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct EmbeddedText {
    pub entity_type: String,
    pub entity_id: String,
    pub model: String,
    /// The surface text that was embedded (used for keyword fallback and
    /// display without re-deriving it from the source table).
    pub text: String,
    pub vector: Vec<f32>,
    pub created_at: String,
    pub updated_at: String,
}

/// Memory domain constants used as `entity_type`.
pub mod entity_kind {
    /// Embeddings of `facts` rows (subject/predicate/object).
    pub const FACT: &str = "fact";
    /// Embeddings of conversation events: user messages and compaction
    /// summaries (the event-stream memory).
    pub const EPISODE: &str = "episode";
}

/// Max unembedded facts returned per missing-ids scan (recent first).
pub const FACT_EMBED_BACKLOG_LIMIT: usize = 128;

/// Max unembedded episode entities (compaction summaries preferred, then
/// recent user messages) returned per missing-ids scan. Prevents a single
/// maintenance / hot-path embed pass from exploding after enabling or
/// switching the embedding model on a large history.
pub const EPISODE_EMBED_BACKLOG_LIMIT: usize = 64;

/// Serialize an f32 vector as a little-endian byte blob for SQLite storage.
pub fn encode_vector(v: &[f32]) -> Vec<u8> {
    v.iter().flat_map(|f| f.to_le_bytes()).collect()
}

/// Deserialize a stored little-endian f32 blob back into a vector.
pub fn decode_vector(blob: &[u8]) -> Vec<f32> {
    blob.chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect()
}

/// Cosine similarity between two vectors. Degenerate inputs (either empty or
/// zero-norm) score 0.0.
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f64 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let mut dot = 0.0f64;
    let mut na = 0.0f64;
    let mut nb = 0.0f64;
    for (x, y) in a.iter().zip(b.iter()) {
        dot += (*x as f64) * (*y as f64);
        na += (*x as f64) * (*x as f64);
        nb += (*y as f64) * (*y as f64);
    }
    if na == 0.0 || nb == 0.0 {
        return 0.0;
    }
    dot / (na.sqrt() * nb.sqrt())
}

fn row_to_embedded(row: &rusqlite::Row) -> rusqlite::Result<EmbeddedText> {
    let vector_blob: Vec<u8> = row.get(3)?;
    Ok(EmbeddedText {
        entity_type: row.get(0)?,
        entity_id: row.get(1)?,
        model: row.get(2)?,
        vector: decode_vector(&vector_blob),
        text: row.get(4)?,
        created_at: row.get(5)?,
        updated_at: row.get(6)?,
    })
}

const EMBED_COLS: &str = "entity_type, entity_id, model, vector, text, created_at, updated_at";

impl Database {
    /// Insert or replace the embedding for an (entity_type, entity_id) under
    /// the given model. Re-embedding the same entity updates the vector and
    /// surface text in place.
    pub fn save_embedding(
        &self,
        entity_type: &str,
        entity_id: &str,
        model: &str,
        vector: &[f32],
        text: &str,
    ) -> anyhow::Result<()> {
        let now = chrono::Utc::now().to_rfc3339();
        let blob = encode_vector(vector);
        let conn = self.conn();
        conn.execute(
            "INSERT INTO memory_embeddings (entity_type, entity_id, model, vector, text, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)
             ON CONFLICT(entity_type, entity_id, model)
             DO UPDATE SET vector = excluded.vector, text = excluded.text, updated_at = excluded.updated_at",
            rusqlite::params![entity_type, entity_id, model, blob, text, now],
        )?;
        self.cache_invalidate_embeddings(entity_type);
        Ok(())
    }

    pub fn get_embedding(
        &self,
        entity_type: &str,
        entity_id: &str,
    ) -> anyhow::Result<Option<EmbeddedText>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!(
            "SELECT {EMBED_COLS} FROM memory_embeddings WHERE entity_type = ?1 AND entity_id = ?2"
        ))?;
        let mut rows = stmt.query(rusqlite::params![entity_type, entity_id])?;
        match rows.next()? {
            Some(row) => Ok(Some(row_to_embedded(row)?)),
            None => Ok(None),
        }
    }

    /// All stored embeddings of one domain, newest first. Cached per domain
    /// (the vector index is small and read far more often than written), so
    /// brute-force recall does not re-read + decode the whole table per query.
    pub fn list_embeddings(&self, entity_type: &str) -> anyhow::Result<Vec<EmbeddedText>> {
        if let Some(cached) = self.cache_get_embeddings(entity_type) {
            return Ok(cached);
        }
        let key = format!("_embeddings_{}", entity_type);
        let cache_gen = self.cache_generation(&key);
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!(
            "SELECT {EMBED_COLS} FROM memory_embeddings WHERE entity_type = ?1
             ORDER BY updated_at DESC"
        ))?;
        let rows = stmt.query_map(rusqlite::params![entity_type], row_to_embedded)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        self.cache_put_embeddings(entity_type, out.clone(), 60, cache_gen);
        Ok(out)
    }

    /// Entity ids of one domain that have no embedding yet, capped so a
    /// single catch-up pass stays bounded. Prefer recent rows; for episodes,
    /// compaction summaries are taken before raw user messages (higher signal
    /// per embed). Repeated passes drain the backlog newest→oldest.
    pub fn missing_embedding_ids(&self, entity_type: &str) -> anyhow::Result<Vec<String>> {
        let limit = match entity_type {
            entity_kind::FACT => FACT_EMBED_BACKLOG_LIMIT,
            entity_kind::EPISODE => EPISODE_EMBED_BACKLOG_LIMIT,
            _ => return Ok(Vec::new()),
        };
        self.missing_embedding_ids_limited(entity_type, limit)
    }

    /// Like [`Self::missing_embedding_ids`] with an explicit cap (tests / tuning).
    pub fn missing_embedding_ids_limited(
        &self,
        entity_type: &str,
        limit: usize,
    ) -> anyhow::Result<Vec<String>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let conn = self.conn();
        match entity_type {
            entity_kind::FACT => {
                let mut stmt = conn.prepare(
                    "SELECT id FROM facts
                     WHERE id NOT IN (
                         SELECT entity_id FROM memory_embeddings WHERE entity_type = ?1
                     )
                     ORDER BY COALESCE(last_seen_at, created_at) DESC
                     LIMIT ?2",
                )?;
                let rows = stmt.query_map(rusqlite::params![entity_type, limit as i64], |r| {
                    r.get::<_, String>(0)
                })?;
                let mut out = Vec::new();
                for row in rows {
                    out.push(row?);
                }
                Ok(out)
            }
            entity_kind::EPISODE => {
                // Summaries first, then recent user messages to fill the rest.
                // Both queries share this connection guard (non-reentrant mutex).
                let mut out: Vec<String> = Vec::new();
                let mut ep_stmt = conn.prepare(
                    "SELECT id FROM memory_episodes
                     WHERE id NOT IN (
                         SELECT entity_id FROM memory_embeddings WHERE entity_type = ?1
                     )
                     ORDER BY created_at DESC
                     LIMIT ?2",
                )?;
                let ep_rows =
                    ep_stmt.query_map(rusqlite::params![entity_type, limit as i64], |r| {
                        r.get::<_, String>(0)
                    })?;
                for row in ep_rows {
                    out.push(row?);
                }
                let remaining = limit.saturating_sub(out.len());
                if remaining > 0 {
                    let mut stmt = conn.prepare(
                        "SELECT id FROM messages
                         WHERE role = 'user'
                           AND id NOT IN (
                               SELECT entity_id FROM memory_embeddings WHERE entity_type = ?1
                           )
                         ORDER BY created_at DESC
                         LIMIT ?2",
                    )?;
                    let rows =
                        stmt.query_map(rusqlite::params![entity_type, remaining as i64], |r| {
                            r.get::<_, String>(0)
                        })?;
                    for row in rows {
                        out.push(row?);
                    }
                }
                Ok(out)
            }
            _ => Ok(Vec::new()),
        }
    }

    /// Embeddable surface text for a fact: `subject predicate object`.
    pub fn fact_text_by_id(&self, fact_id: &str) -> anyhow::Result<Option<String>> {
        let conn = self.conn();
        let text = match conn.query_row(
            "SELECT subject || ' ' || predicate || ' ' || object FROM facts WHERE id = ?1",
            rusqlite::params![fact_id],
            |r| r.get(0),
        ) {
            Ok(t) => Some(t),
            Err(rusqlite::Error::QueryReturnedNoRows) => None,
            Err(e) => return Err(e.into()),
        };
        Ok(text)
    }

    /// Source text for an episode entity: the user message content, or the
    /// compaction summary when the id belongs to a `memory_episodes` row.
    pub fn episode_text(&self, entity_id: &str) -> anyhow::Result<Option<String>> {
        let conn = self.conn();
        let content = match conn.query_row(
            "SELECT content FROM messages WHERE id = ?1",
            rusqlite::params![entity_id],
            |r| r.get::<_, String>(0),
        ) {
            Ok(c) => Some(c),
            Err(rusqlite::Error::QueryReturnedNoRows) => None,
            Err(e) => return Err(e.into()),
        };
        if let Some(content) = content {
            return Ok(Some(content));
        }
        let summary = match conn.query_row(
            "SELECT summary FROM memory_episodes WHERE id = ?1",
            rusqlite::params![entity_id],
            |r| r.get::<_, String>(0),
        ) {
            Ok(s) => Some(s),
            Err(rusqlite::Error::QueryReturnedNoRows) => None,
            Err(e) => return Err(e.into()),
        };
        Ok(summary)
    }

    /// Brute-force cosine search over one memory domain. Data volumes here are
    /// small (hundreds of facts/episodes), so a linear scan is fast and avoids
    /// a native ANN dependency. Returns up to `limit` hits ordered by
    /// descending similarity.
    pub fn search_embeddings(
        &self,
        entity_type: &str,
        query_vec: &[f32],
        limit: usize,
    ) -> anyhow::Result<Vec<(EmbeddedText, f64)>> {
        let mut hits: Vec<(EmbeddedText, f64)> = self
            .list_embeddings(entity_type)?
            .into_iter()
            .map(|e| {
                let score = cosine_similarity(query_vec, &e.vector);
                (e, score)
            })
            .collect();
        hits.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        hits.truncate(limit);
        Ok(hits)
    }

    /// Keyword search over the event-stream memory (user messages plus
    /// persisted compaction summaries), independent of the vector index — so
    /// cross-session recall works even when no `embedding_model` is configured.
    /// Terms are matched as case-insensitive substrings; results are ranked by
    /// the number of distinct terms matched, then recency.
    pub fn search_episodes_by_keywords(
        &self,
        terms: &[&str],
        limit: usize,
    ) -> anyhow::Result<Vec<String>> {
        let terms: Vec<&str> = terms.iter().filter(|t| !t.is_empty()).copied().collect();
        if terms.is_empty() {
            return Ok(Vec::new());
        }
        let conn = self.conn();
        // Candidate pool is bounded to the most recent episodes (the oldest
        // 1000 user messages by creation time, plus every stored compaction
        // summary), matching how the vector index behaves.
        let mut stmt = conn.prepare(
            "SELECT content, created_at FROM (
                 SELECT content, created_at FROM messages WHERE role = 'user'
                 UNION ALL
                 SELECT summary, created_at FROM memory_episodes
             )
             ORDER BY created_at DESC LIMIT 1000",
        )?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
        let lower_terms: Vec<String> = terms.iter().map(|t| t.to_lowercase()).collect();
        let mut scored: Vec<(usize, String)> = Vec::new();
        for row in rows {
            let (content, _created) = row?;
            let tl = content.to_lowercase();
            let hits = lower_terms
                .iter()
                .filter(|term| tl.contains(term.as_str()))
                .count();
            if hits > 0 {
                scored.push((hits, content));
            }
        }
        scored.sort_by_key(|(hits, _)| std::cmp::Reverse(*hits));
        scored.truncate(limit);
        Ok(scored.into_iter().map(|(_, t)| t).collect())
    }

    /// Distinct embedding model names currently in the vector index.
    pub fn list_embedding_models(&self) -> anyhow::Result<Vec<String>> {
        let conn = self.conn();
        let mut stmt = conn.prepare("SELECT DISTINCT model FROM memory_embeddings")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Drop every stored embedding. Used when the configured embedding model
    /// changes: vectors from a different model are not comparable (dimension
    /// mismatch makes cosine similarity degenerate), so the index is rebuilt
    /// from scratch on the next embedding pass.
    pub fn clear_embeddings(&self) -> anyhow::Result<u64> {
        let conn = self.conn();
        let deleted = conn.execute("DELETE FROM memory_embeddings", [])? as u64;
        self.cache_invalidate_embeddings(entity_kind::FACT);
        self.cache_invalidate_embeddings(entity_kind::EPISODE);
        Ok(deleted)
    }

    /// Remove embeddings whose owning entity no longer exists (facts deleted,
    /// messages pruned). Keeps the index from growing unbounded around
    /// pruned memory.
    pub fn prune_orphaned_embeddings(&self) -> anyhow::Result<u64> {
        let conn = self.conn();
        let deleted = conn.execute(
            "DELETE FROM memory_embeddings WHERE
                (entity_type = 'fact' AND entity_id NOT IN (SELECT id FROM facts))
             OR (entity_type = 'episode'
                 AND entity_id NOT IN (SELECT id FROM messages)
                 AND entity_id NOT IN (SELECT id FROM memory_episodes))",
            [],
        )? as u64;
        if deleted > 0 {
            self.cache_invalidate_embeddings(entity_kind::FACT);
            self.cache_invalidate_embeddings(entity_kind::EPISODE);
        }
        Ok(deleted)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Database;

    fn db() -> Database {
        Database::open_in_memory().unwrap()
    }

    #[test]
    fn vector_roundtrip() {
        let v = vec![0.5f32, -1.25, 3.0, 0.0];
        let blob = encode_vector(&v);
        assert_eq!(decode_vector(&blob), v);
    }

    #[test]
    fn cosine_identical_is_one() {
        let v = vec![1.0f32, 2.0, 3.0];
        assert!((cosine_similarity(&v, &v) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn cosine_orthogonal_is_zero() {
        let a = vec![1.0f32, 0.0];
        let b = vec![0.0f32, 1.0];
        assert!((cosine_similarity(&a, &b)).abs() < 1e-9);
    }

    #[test]
    fn cosine_degenerate_is_zero() {
        assert_eq!(cosine_similarity(&[], &[1.0]), 0.0);
        assert_eq!(cosine_similarity(&[0.0, 0.0], &[1.0, 1.0]), 0.0);
    }

    #[test]
    fn save_and_get_embedding() {
        let db = db();
        db.save_embedding(
            entity_kind::FACT,
            "f1",
            "test-emb",
            &[0.1, 0.2],
            "name: Alice",
        )
        .unwrap();
        let got = db.get_embedding(entity_kind::FACT, "f1").unwrap().unwrap();
        assert_eq!(got.entity_id, "f1");
        assert_eq!(got.vector, vec![0.1, 0.2]);
        assert_eq!(got.text, "name: Alice");
    }

    #[test]
    fn save_replaces_existing() {
        let db = db();
        db.save_embedding(entity_kind::FACT, "f1", "m", &[1.0], "a")
            .unwrap();
        db.save_embedding(entity_kind::FACT, "f1", "m", &[2.0, 3.0], "b")
            .unwrap();
        let got = db.get_embedding(entity_kind::FACT, "f1").unwrap().unwrap();
        assert_eq!(got.vector, vec![2.0, 3.0]);
        assert_eq!(got.text, "b");
    }

    #[test]
    fn search_ranks_by_similarity() {
        let db = db();
        db.save_embedding(entity_kind::FACT, "f1", "m", &[1.0, 0.0], "a")
            .unwrap();
        db.save_embedding(entity_kind::FACT, "f2", "m", &[0.0, 1.0], "b")
            .unwrap();
        let hits = db
            .search_embeddings(entity_kind::FACT, &[1.0, 0.0], 10)
            .unwrap();
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].0.entity_id, "f1");
        assert!(hits[0].1 > hits[1].1);
    }

    #[test]
    fn search_respects_limit_and_domain() {
        let db = db();
        db.save_embedding(entity_kind::FACT, "f1", "m", &[1.0, 0.0], "a")
            .unwrap();
        db.save_embedding(entity_kind::EPISODE, "e1", "m", &[1.0, 0.0], "b")
            .unwrap();
        let hits = db
            .search_embeddings(entity_kind::FACT, &[1.0, 0.0], 1)
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].0.entity_id, "f1");
    }

    #[test]
    fn missing_embedding_ids_facts() {
        let db = db();
        db.insert_fact("user", "likes", "Rust", "user", 0.9, &["preference"])
            .unwrap();
        let f = db
            .insert_fact("user", "likes", "Go", "user", 0.8, &["preference"])
            .unwrap();
        db.save_embedding(entity_kind::FACT, &f.id, "m", &[1.0], "x")
            .unwrap();
        let missing = db.missing_embedding_ids(entity_kind::FACT).unwrap();
        assert_eq!(missing.len(), 1);
    }

    #[test]
    fn missing_embedding_ids_episodes() {
        let db = db();
        let session = db.create_session("t", "").unwrap();
        let msg = db
            .add_message(&session.id, "user", "hello world", Some("text"), None)
            .unwrap();
        let missing = db.missing_embedding_ids(entity_kind::EPISODE).unwrap();
        assert_eq!(missing.len(), 1);
        assert!(missing.contains(&msg.id));
        db.save_embedding(entity_kind::EPISODE, &msg.id, "m", &[1.0], "hello world")
            .unwrap();
        let missing = db.missing_embedding_ids(entity_kind::EPISODE).unwrap();
        assert_eq!(missing.len(), 0);
    }

    #[test]
    fn missing_embedding_ids_episodes_respects_limit_and_prefers_summaries() {
        let db = db();
        let session = db.create_session("t", "").unwrap();
        for i in 0..5 {
            db.add_message(
                &session.id,
                "user",
                &format!("msg-{i}"),
                Some("text"),
                None,
            )
            .unwrap();
        }
        let ep_a = db.add_episode(&session.id, "summary-a").unwrap();
        let ep_b = db.add_episode(&session.id, "summary-b").unwrap();

        let missing = db
            .missing_embedding_ids_limited(entity_kind::EPISODE, 2)
            .unwrap();
        assert_eq!(missing.len(), 2);
        assert!(
            missing.contains(&ep_a) && missing.contains(&ep_b),
            "summaries must fill the cap before raw user messages, got {:?}",
            missing
        );

        let missing3 = db
            .missing_embedding_ids_limited(entity_kind::EPISODE, 3)
            .unwrap();
        assert_eq!(missing3.len(), 3);
        assert!(missing3.contains(&ep_a) && missing3.contains(&ep_b));
        // Third slot is a recent user message (not another summary).
        assert_eq!(
            missing3
                .iter()
                .filter(|id| *id != &ep_a && *id != &ep_b)
                .count(),
            1
        );
    }

    #[test]
    fn episode_text_resolves_message() {
        let db = db();
        let session = db.create_session("t", "").unwrap();
        let msg = db
            .add_message(&session.id, "user", "remember this", Some("text"), None)
            .unwrap();
        assert_eq!(
            db.episode_text(&msg.id).unwrap(),
            Some("remember this".into())
        );
        assert_eq!(db.episode_text("nope").unwrap(), None);
    }

    #[test]
    fn prune_removes_orphaned() {
        let db = db();
        let session = db.create_session("t", "").unwrap();
        let msg = db
            .add_message(&session.id, "user", "hello", Some("text"), None)
            .unwrap();
        db.save_embedding(entity_kind::EPISODE, &msg.id, "m", &[1.0], "hello")
            .unwrap();
        db.save_embedding(entity_kind::EPISODE, "ghost", "m", &[1.0], "gone")
            .unwrap();
        db.save_embedding(entity_kind::FACT, "ghost-fact", "m", &[1.0], "gone")
            .unwrap();
        let deleted = db.prune_orphaned_embeddings().unwrap();
        assert_eq!(deleted, 2);
        assert!(
            db.get_embedding(entity_kind::EPISODE, &msg.id)
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn list_embedding_models_distinct() {
        let db = db();
        db.save_embedding(entity_kind::FACT, "f1", "model-a", &[1.0], "x")
            .unwrap();
        db.save_embedding(entity_kind::FACT, "f2", "model-a", &[1.0], "y")
            .unwrap();
        db.save_embedding(entity_kind::EPISODE, "e1", "model-b", &[1.0], "z")
            .unwrap();
        let models = db.list_embedding_models().unwrap();
        assert_eq!(models.len(), 2);
        assert!(models.contains(&"model-a".to_string()));
        assert!(models.contains(&"model-b".to_string()));
    }

    #[test]
    fn clear_embeddings_drops_everything() {
        let db = db();
        db.save_embedding(entity_kind::FACT, "f1", "m", &[1.0], "x")
            .unwrap();
        db.save_embedding(entity_kind::EPISODE, "e1", "m", &[1.0], "y")
            .unwrap();
        let cleared = db.clear_embeddings().unwrap();
        assert_eq!(cleared, 2);
        assert!(db.list_embeddings(entity_kind::FACT).unwrap().is_empty());
        assert!(db.list_embeddings(entity_kind::EPISODE).unwrap().is_empty());
    }

    #[test]
    fn search_episodes_by_keywords_ranks_matches() {
        let db = db();
        let session = db.create_session("t", "").unwrap();
        db.add_message(
            &session.id,
            "user",
            "I discussed the dark theme preference earlier",
            Some("text"),
            None,
        )
        .unwrap();
        db.add_message(
            &session.id,
            "user",
            "unrelated note about groceries",
            Some("text"),
            None,
        )
        .unwrap();
        let hits = db
            .search_episodes_by_keywords(&["dark", "theme"], 5)
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert!(hits[0].contains("dark theme preference"));
    }

    #[test]
    fn search_episodes_by_keywords_empty_terms_returns_nothing() {
        let db = db();
        let session = db.create_session("t", "").unwrap();
        db.add_message(&session.id, "user", "hello world", Some("text"), None)
            .unwrap();
        assert!(db.search_episodes_by_keywords(&[], 5).unwrap().is_empty());
    }

    #[test]
    fn episodes_include_compaction_summaries() {
        let db = db();
        let session = db.create_session("t", "").unwrap();
        db.add_message(&session.id, "user", "plain message", Some("text"), None)
            .unwrap();
        let ep = db
            .add_episode(&session.id, "user prefers the dark theme everywhere")
            .unwrap();

        // Summaries are missing-index candidates and resolve their text.
        let missing = db.missing_embedding_ids(entity_kind::EPISODE).unwrap();
        assert!(missing.contains(&ep));
        assert!(missing.iter().any(|m| m != &ep));
        assert_eq!(
            db.episode_text(&ep).unwrap().as_deref(),
            Some("user prefers the dark theme everywhere")
        );

        // Keyword search surfaces the summary text.
        let hits = db
            .search_episodes_by_keywords(&["dark", "theme"], 5)
            .unwrap();
        assert!(hits.iter().any(|h| h.contains("dark theme")));

        // Indexing the episode removes it from the missing set and pruning
        // does not treat it as orphaned.
        db.save_embedding(entity_kind::EPISODE, &ep, "m", &[1.0, 0.0], "x")
            .unwrap();
        let missing = db.missing_embedding_ids(entity_kind::EPISODE).unwrap();
        assert!(!missing.contains(&ep));
        assert_eq!(db.prune_orphaned_embeddings().unwrap(), 0);
    }

    #[test]
    fn embedding_list_is_cached_and_invalidated_on_write() {
        let db = db();
        assert!(db.list_embeddings(entity_kind::FACT).unwrap().is_empty());
        // First read caches; second read hits the cache (still correct).
        assert!(db.list_embeddings(entity_kind::FACT).unwrap().is_empty());
        // A write invalidates the cache so the next read sees the new row.
        db.save_embedding(entity_kind::FACT, "f1", "m", &[1.0], "x")
            .unwrap();
        let all = db.list_embeddings(entity_kind::FACT).unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].entity_id, "f1");
        // Bulk clear invalidates again.
        assert_eq!(db.clear_embeddings().unwrap(), 1);
        assert!(db.list_embeddings(entity_kind::FACT).unwrap().is_empty());
    }

    #[test]
    fn fact_embedding_invalidated_by_fact_update_and_delete() {
        let db = db();
        let fact = db
            .insert_fact(
                "user",
                "language",
                "English",
                "inferred",
                0.9,
                &["preference"],
            )
            .unwrap();
        db.save_embedding(
            entity_kind::FACT,
            &fact.id,
            "m",
            &[1.0],
            "user language English",
        )
        .unwrap();
        // Single-valued correction: the old row is demoted by an UPDATE,
        // which must drop its stale vector.
        db.upsert_fact_with_durability(
            "user",
            "language",
            "Chinese",
            "inferred",
            0.9,
            &["preference"],
            None,
            1.0,
        )
        .unwrap();
        assert!(
            db.get_embedding(entity_kind::FACT, &fact.id)
                .unwrap()
                .is_none(),
            "corrected fact must lose its stale embedding"
        );
        // Re-embed, then DELETE drops it again.
        db.save_embedding(
            entity_kind::FACT,
            &fact.id,
            "m",
            &[1.0],
            "user language English",
        )
        .unwrap();
        db.delete_fact(&fact.id).unwrap();
        assert!(
            db.get_embedding(entity_kind::FACT, &fact.id)
                .unwrap()
                .is_none(),
            "deleted fact must lose its embedding"
        );
    }

    #[test]
    fn embedding_list_cache_invalidated_by_fact_mutation() {
        let db = db();
        let fact = db
            .insert_fact("user", "likes", "Rust", "inferred", 0.9, &["preference"])
            .unwrap();
        db.save_embedding(entity_kind::FACT, &fact.id, "m", &[1.0], "x")
            .unwrap();
        // Warm the embeddings list cache.
        assert_eq!(db.list_embeddings(entity_kind::FACT).unwrap().len(), 1);
        // The facts_embed_del trigger deletes the embedding row directly in
        // SQL; the cache must be invalidated too, or recall keeps serving the
        // stale vector for the whole TTL.
        db.delete_fact(&fact.id).unwrap();
        assert!(
            db.list_embeddings(entity_kind::FACT).unwrap().is_empty(),
            "fact mutation must invalidate the embeddings list cache"
        );
    }
}
