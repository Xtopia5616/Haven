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

/// Cap on embeddings scored per brute-force search when no tighter domain
/// filter applies (P1-4). Prefer newest rows; sqlite-vec is deferred until
/// fact volume reaches ~10k.
pub const EMBEDDING_SEARCH_SCAN_CAP: usize = 256;

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
        self.get_embedding_for_model(entity_type, entity_id, None)
    }

    /// Like [`Self::get_embedding`], optionally restricted to one model
    /// (P2-13). Pass `Some(model)` so mixed-model rows cannot be returned.
    pub fn get_embedding_for_model(
        &self,
        entity_type: &str,
        entity_id: &str,
        model: Option<&str>,
    ) -> anyhow::Result<Option<EmbeddedText>> {
        let conn = self.conn();
        if let Some(model) = model.filter(|m| !m.is_empty()) {
            let mut stmt = conn.prepare(&format!(
                "SELECT {EMBED_COLS} FROM memory_embeddings
                 WHERE entity_type = ?1 AND entity_id = ?2 AND model = ?3"
            ))?;
            let mut rows = stmt.query(rusqlite::params![entity_type, entity_id, model])?;
            return match rows.next()? {
                Some(row) => Ok(Some(row_to_embedded(row)?)),
                None => Ok(None),
            };
        }
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
    ///
    /// When `model` is non-empty, only embeddings for that model count
    /// (P2-13) — so a failed `clear_embeddings` after a model switch still
    /// re-embeds under the new model.
    pub fn missing_embedding_ids(
        &self,
        entity_type: &str,
        model: &str,
    ) -> anyhow::Result<Vec<String>> {
        let limit = match entity_type {
            entity_kind::FACT => FACT_EMBED_BACKLOG_LIMIT,
            entity_kind::EPISODE => EPISODE_EMBED_BACKLOG_LIMIT,
            _ => return Ok(Vec::new()),
        };
        self.missing_embedding_ids_limited(entity_type, model, limit)
    }

    /// Like [`Self::missing_embedding_ids`] with an explicit cap (tests / tuning).
    /// Empty `model` treats any stored embedding as covering the entity
    /// (legacy test helper); production callers pass the current model name.
    pub fn missing_embedding_ids_limited(
        &self,
        entity_type: &str,
        model: &str,
        limit: usize,
    ) -> anyhow::Result<Vec<String>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let conn = self.conn();
        let model_filter = !model.is_empty();
        match entity_type {
            entity_kind::FACT => {
                let mut out = Vec::new();
                if model_filter {
                    let mut stmt = conn.prepare(
                        "SELECT id FROM facts
                         WHERE id NOT IN (
                             SELECT entity_id FROM memory_embeddings
                             WHERE entity_type = ?1 AND model = ?2
                         )
                         ORDER BY COALESCE(last_seen_at, created_at) DESC
                         LIMIT ?3",
                    )?;
                    for row in stmt.query_map(
                        rusqlite::params![entity_type, model, limit as i64],
                        |r| r.get::<_, String>(0),
                    )? {
                        out.push(row?);
                    }
                } else {
                    let mut stmt = conn.prepare(
                        "SELECT id FROM facts
                         WHERE id NOT IN (
                             SELECT entity_id FROM memory_embeddings WHERE entity_type = ?1
                         )
                         ORDER BY COALESCE(last_seen_at, created_at) DESC
                         LIMIT ?2",
                    )?;
                    for row in stmt
                        .query_map(rusqlite::params![entity_type, limit as i64], |r| {
                            r.get::<_, String>(0)
                        })?
                    {
                        out.push(row?);
                    }
                }
                Ok(out)
            }
            entity_kind::EPISODE => {
                // Summaries first, then recent user messages to fill the rest.
                // Both queries share this connection guard (non-reentrant mutex).
                let mut out: Vec<String> = Vec::new();
                if model_filter {
                    let mut ep_stmt = conn.prepare(
                        "SELECT id FROM memory_episodes
                         WHERE id NOT IN (
                             SELECT entity_id FROM memory_embeddings
                             WHERE entity_type = ?1 AND model = ?2
                         )
                         ORDER BY created_at DESC
                         LIMIT ?3",
                    )?;
                    for row in ep_stmt.query_map(
                        rusqlite::params![entity_type, model, limit as i64],
                        |r| r.get::<_, String>(0),
                    )? {
                        out.push(row?);
                    }
                } else {
                    let mut ep_stmt = conn.prepare(
                        "SELECT id FROM memory_episodes
                         WHERE id NOT IN (
                             SELECT entity_id FROM memory_embeddings WHERE entity_type = ?1
                         )
                         ORDER BY created_at DESC
                         LIMIT ?2",
                    )?;
                    for row in ep_stmt
                        .query_map(rusqlite::params![entity_type, limit as i64], |r| {
                            r.get::<_, String>(0)
                        })?
                    {
                        out.push(row?);
                    }
                }
                let remaining = limit.saturating_sub(out.len());
                if remaining > 0 {
                    if model_filter {
                        let mut stmt = conn.prepare(
                            "SELECT id FROM messages
                             WHERE role = 'user'
                               AND id NOT IN (
                                   SELECT entity_id FROM memory_embeddings
                                   WHERE entity_type = ?1 AND model = ?2
                               )
                             ORDER BY created_at DESC
                             LIMIT ?3",
                        )?;
                        for row in stmt.query_map(
                            rusqlite::params![entity_type, model, remaining as i64],
                            |r| r.get::<_, String>(0),
                        )? {
                            out.push(row?);
                        }
                    } else {
                        let mut stmt = conn.prepare(
                            "SELECT id FROM messages
                             WHERE role = 'user'
                               AND id NOT IN (
                                   SELECT entity_id FROM memory_embeddings WHERE entity_type = ?1
                               )
                             ORDER BY created_at DESC
                             LIMIT ?2",
                        )?;
                        for row in stmt.query_map(
                            rusqlite::params![entity_type, remaining as i64],
                            |r| r.get::<_, String>(0),
                        )? {
                            out.push(row?);
                        }
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

    /// Brute-force cosine search over one memory domain. Prefer
    /// [`Self::search_embeddings_filtered`] when a subject or session scope
    /// is known (P1-4). Always filters by `model` (P2-13); empty model is
    /// fail-closed (no hits). Unfiltered calls score at most
    /// [`EMBEDDING_SEARCH_SCAN_CAP`] newest rows of that model.
    pub fn search_embeddings(
        &self,
        entity_type: &str,
        query_vec: &[f32],
        limit: usize,
        model: &str,
    ) -> anyhow::Result<Vec<(EmbeddedText, f64)>> {
        self.search_embeddings_filtered(entity_type, query_vec, limit, model, None, None)
    }

    /// Cosine search with optional domain narrowing (P1-4) and required model
    /// filter (P2-13):
    /// - `model`: only embeddings from this model; empty → no hits
    /// - `fact_subject`: only embeddings whose fact row has this subject
    /// - `exclude_session_id`: drop episode entities owned by this session
    ///
    /// Candidate set is bounded by `max(limit * 4, EMBEDDING_SEARCH_SCAN_CAP)`
    /// newest matching rows so prompt build stays cheap before sqlite-vec.
    pub fn search_embeddings_filtered(
        &self,
        entity_type: &str,
        query_vec: &[f32],
        limit: usize,
        model: &str,
        fact_subject: Option<&str>,
        exclude_session_id: Option<&str>,
    ) -> anyhow::Result<Vec<(EmbeddedText, f64)>> {
        if limit == 0 || model.is_empty() {
            return Ok(Vec::new());
        }
        let scan_cap = (limit.saturating_mul(4)).max(EMBEDDING_SEARCH_SCAN_CAP);
        let candidates = self.list_embeddings_for_search(
            entity_type,
            model,
            scan_cap,
            fact_subject,
            exclude_session_id,
        )?;
        let mut hits: Vec<(EmbeddedText, f64)> = candidates
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

    /// Newest embeddings for one domain + model, optionally narrowed by fact
    /// subject or episode owning-session exclusion. Used by vector search so
    /// cosine never scans an unbounded or mixed-model table (P2-13).
    fn list_embeddings_for_search(
        &self,
        entity_type: &str,
        model: &str,
        scan_cap: usize,
        fact_subject: Option<&str>,
        exclude_session_id: Option<&str>,
    ) -> anyhow::Result<Vec<EmbeddedText>> {
        // Fast path: cached full list filtered by model when no SQL domain
        // filter is needed and the cache is smaller than the scan cap.
        if fact_subject.is_none()
            && exclude_session_id.is_none()
            && let Some(cached) = self.cache_get_embeddings(entity_type)
        {
            let filtered: Vec<_> = cached.into_iter().filter(|e| e.model == model).collect();
            if filtered.len() <= scan_cap {
                return Ok(filtered);
            }
            return Ok(filtered.into_iter().take(scan_cap).collect());
        }

        let conn = self.conn();
        match (entity_type, fact_subject, exclude_session_id) {
            (entity_kind::FACT, Some(subject), _) => {
                let mut stmt = conn.prepare(
                    "SELECT e.entity_type, e.entity_id, e.model, e.vector, e.text,
                            e.created_at, e.updated_at
                     FROM memory_embeddings e
                     INNER JOIN facts f ON f.id = e.entity_id
                     WHERE e.entity_type = ?1 AND e.model = ?2 AND f.subject = ?3
                     ORDER BY e.updated_at DESC
                     LIMIT ?4",
                )?;
                let rows = stmt.query_map(
                    rusqlite::params![entity_type, model, subject, scan_cap as i64],
                    row_to_embedded,
                )?;
                let mut out = Vec::new();
                for row in rows {
                    out.push(row?);
                }
                Ok(out)
            }
            (entity_kind::EPISODE, _, Some(sid)) => {
                let mut stmt = conn.prepare(
                    "SELECT e.entity_type, e.entity_id, e.model, e.vector, e.text,
                            e.created_at, e.updated_at
                     FROM memory_embeddings e
                     WHERE e.entity_type = ?1 AND e.model = ?2
                       AND e.entity_id NOT IN (
                           SELECT id FROM messages WHERE session_id = ?3
                           UNION ALL
                           SELECT id FROM memory_episodes WHERE session_id = ?3
                       )
                     ORDER BY e.updated_at DESC
                     LIMIT ?4",
                )?;
                let rows = stmt.query_map(
                    rusqlite::params![entity_type, model, sid, scan_cap as i64],
                    row_to_embedded,
                )?;
                let mut out = Vec::new();
                for row in rows {
                    out.push(row?);
                }
                Ok(out)
            }
            _ => {
                let mut stmt = conn.prepare(&format!(
                    "SELECT {EMBED_COLS} FROM memory_embeddings
                     WHERE entity_type = ?1 AND model = ?2
                     ORDER BY updated_at DESC
                     LIMIT ?3"
                ))?;
                let rows = stmt.query_map(
                    rusqlite::params![entity_type, model, scan_cap as i64],
                    row_to_embedded,
                )?;
                let mut out = Vec::new();
                for row in rows {
                    out.push(row?);
                }
                Ok(out)
            }
        }
    }

    /// Keyword search over the event-stream memory (user messages plus
    /// persisted compaction summaries), independent of the vector index — so
    /// cross-session recall works even when no `embedding_model` is configured.
    /// Compaction summaries use `episodes_fts` (trigram) when available
    /// (P2-10 / L6); user messages still use a bounded LIKE/substring scan.
    /// Results are ranked by distinct term hits, then recency.
    ///
    /// When `exclude_session_id` is set (Phase 6 / S2), rows from that session
    /// are omitted so the current conversation is not recalled as "past".
    pub fn search_episodes_by_keywords(
        &self,
        terms: &[&str],
        limit: usize,
    ) -> anyhow::Result<Vec<String>> {
        self.search_episodes_by_keywords_excluding(terms, limit, None)
    }

    /// Like [`Self::search_episodes_by_keywords`], with optional same-session exclusion.
    pub fn search_episodes_by_keywords_excluding(
        &self,
        terms: &[&str],
        limit: usize,
        exclude_session_id: Option<&str>,
    ) -> anyhow::Result<Vec<String>> {
        let terms: Vec<&str> = terms.iter().filter(|t| !t.is_empty()).copied().collect();
        if terms.is_empty() || limit == 0 {
            return Ok(Vec::new());
        }
        let lower_terms: Vec<String> = terms.iter().map(|t| t.to_lowercase()).collect();
        let mut scored: Vec<(usize, String, String)> = Vec::new();
        let mut seen = std::collections::HashSet::new();

        // P2-10: prefer FTS for compaction summaries (+ topics/entities).
        if let Ok(Some(fts_hits)) =
            self.search_episode_summaries_fts(&terms, exclude_session_id, limit.saturating_mul(4))
        {
            for (display, haystack, created) in fts_hits {
                Self::score_episode_candidate_haystack(
                    &display,
                    &haystack,
                    &created,
                    &lower_terms,
                    &mut scored,
                    &mut seen,
                );
            }
        } else {
            // FTS unavailable: score recent summaries (incl. topics/entities).
            for (display, haystack, created) in
                self.list_recent_episode_rows(exclude_session_id, 1000)?
            {
                Self::score_episode_candidate_haystack(
                    &display,
                    &haystack,
                    &created,
                    &lower_terms,
                    &mut scored,
                    &mut seen,
                );
            }
        }

        // Short terms miss trigram — union recent summaries for digrams only.
        let short: Vec<&str> = terms
            .iter()
            .copied()
            .filter(|t| {
                let n = t.chars().count();
                n > 0 && n < 3
            })
            .collect();
        if !short.is_empty() {
            let short_lower: Vec<String> = short.iter().map(|t| t.to_lowercase()).collect();
            for (display, haystack, created) in
                self.list_recent_episode_rows(exclude_session_id, 1000)?
            {
                let hay = haystack.to_lowercase();
                if short_lower.iter().any(|p| hay.contains(p)) {
                    Self::score_episode_candidate_haystack(
                        &display,
                        &haystack,
                        &created,
                        &lower_terms,
                        &mut scored,
                        &mut seen,
                    );
                }
            }
        }

        // User messages remain a bounded substring scan (not in episodes_fts).
        for (content, created) in self.list_recent_user_messages(exclude_session_id, 1000)? {
            Self::score_episode_candidate(
                &content,
                &created,
                &lower_terms,
                &mut scored,
                &mut seen,
            );
        }

        scored.sort_by(|a, b| {
            b.0.cmp(&a.0)
                .then_with(|| b.2.cmp(&a.2)) // newer created_at first
        });
        scored.truncate(limit);
        Ok(scored.into_iter().map(|(_, t, _)| t).collect())
    }

    fn score_episode_candidate(
        content: &str,
        created: &str,
        lower_terms: &[String],
        scored: &mut Vec<(usize, String, String)>,
        seen: &mut std::collections::HashSet<String>,
    ) {
        Self::score_episode_candidate_haystack(
            content, content, created, lower_terms, scored, seen,
        );
    }

    fn score_episode_candidate_haystack(
        display: &str,
        haystack: &str,
        created: &str,
        lower_terms: &[String],
        scored: &mut Vec<(usize, String, String)>,
        seen: &mut std::collections::HashSet<String>,
    ) {
        if !seen.insert(display.to_string()) {
            return;
        }
        let tl = haystack.to_lowercase();
        let hits = lower_terms
            .iter()
            .filter(|term| tl.contains(term.as_str()))
            .count();
        if hits > 0 {
            scored.push((hits, display.to_string(), created.to_string()));
        }
    }

    fn build_episode_fts_query(terms: &[&str]) -> String {
        terms
            .iter()
            .filter(|t| !t.is_empty())
            .map(|t| format!("\"{}\"", t.replace('"', "\"\"")))
            .collect::<Vec<_>>()
            .join(" OR ")
    }

    /// `Ok(None)` = FTS missing/failed; `Ok(Some(_))` = successful MATCH.
    /// Rows are `(display_summary, search_haystack, created_at)`.
    fn search_episode_summaries_fts(
        &self,
        terms: &[&str],
        exclude_session_id: Option<&str>,
        limit: usize,
    ) -> anyhow::Result<Option<Vec<(String, String, String)>>> {
        if limit == 0 || terms.is_empty() {
            return Ok(Some(Vec::new()));
        }
        let match_expr = Self::build_episode_fts_query(terms);
        let map_row = |r: &rusqlite::Row| -> rusqlite::Result<(String, String, String)> {
            let summary: String = r.get(0)?;
            let topics: String = r.get(1)?;
            let entities: String = r.get(2)?;
            let created: String = r.get(3)?;
            let haystack = format!("{summary} {topics} {entities}");
            Ok((summary, haystack, created))
        };
        let conn = self.conn();
        let result = if let Some(sid) = exclude_session_id {
            let mut stmt = conn.prepare(
                "SELECT e.summary, e.topics, e.entities, e.created_at
                 FROM episodes_fts
                 JOIN memory_episodes e ON e.rowid = episodes_fts.rowid
                 WHERE episodes_fts MATCH ?1 AND e.session_id != ?2
                 ORDER BY bm25(episodes_fts), e.created_at DESC
                 LIMIT ?3",
            );
            match stmt {
                Ok(ref mut s) => s
                    .query_map(rusqlite::params![match_expr, sid, limit as i64], map_row)
                    .and_then(|rows| rows.collect::<Result<Vec<_>, _>>()),
                Err(e) => Err(e),
            }
        } else {
            let mut stmt = conn.prepare(
                "SELECT e.summary, e.topics, e.entities, e.created_at
                 FROM episodes_fts
                 JOIN memory_episodes e ON e.rowid = episodes_fts.rowid
                 WHERE episodes_fts MATCH ?1
                 ORDER BY bm25(episodes_fts), e.created_at DESC
                 LIMIT ?2",
            );
            match stmt {
                Ok(ref mut s) => s
                    .query_map(rusqlite::params![match_expr, limit as i64], map_row)
                    .and_then(|rows| rows.collect::<Result<Vec<_>, _>>()),
                Err(e) => Err(e),
            }
        };
        match result {
            Ok(rows) => Ok(Some(rows)),
            Err(_) => Ok(None),
        }
    }

    /// `(display_summary, search_haystack, created_at)` — haystack includes
    /// topics/entities JSON so structured tags are keyword-visible (P2-10).
    fn list_recent_episode_rows(
        &self,
        exclude_session_id: Option<&str>,
        limit: usize,
    ) -> anyhow::Result<Vec<(String, String, String)>> {
        let conn = self.conn();
        let map_row = |r: &rusqlite::Row| -> rusqlite::Result<(String, String, String)> {
            let summary: String = r.get(0)?;
            let topics: String = r.get(1)?;
            let entities: String = r.get(2)?;
            let created: String = r.get(3)?;
            let haystack = format!("{summary} {topics} {entities}");
            Ok((summary, haystack, created))
        };
        if let Some(sid) = exclude_session_id {
            let mut stmt = conn.prepare(
                "SELECT summary, topics, entities, created_at FROM memory_episodes
                 WHERE session_id != ?1
                 ORDER BY created_at DESC LIMIT ?2",
            )?;
            let rows =
                stmt.query_map(rusqlite::params![sid, limit as i64], map_row)?;
            Ok(rows.collect::<Result<Vec<_>, _>>()?)
        } else {
            let mut stmt = conn.prepare(
                "SELECT summary, topics, entities, created_at FROM memory_episodes
                 ORDER BY created_at DESC LIMIT ?1",
            )?;
            let rows = stmt.query_map(rusqlite::params![limit as i64], map_row)?;
            Ok(rows.collect::<Result<Vec<_>, _>>()?)
        }
    }

    fn list_recent_user_messages(
        &self,
        exclude_session_id: Option<&str>,
        limit: usize,
    ) -> anyhow::Result<Vec<(String, String)>> {
        let conn = self.conn();
        if let Some(sid) = exclude_session_id {
            let mut stmt = conn.prepare(
                "SELECT content, created_at FROM messages
                 WHERE role = 'user' AND session_id != ?1
                 ORDER BY created_at DESC LIMIT ?2",
            )?;
            let rows = stmt.query_map(rusqlite::params![sid, limit as i64], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })?;
            Ok(rows.collect::<Result<Vec<_>, _>>()?)
        } else {
            let mut stmt = conn.prepare(
                "SELECT content, created_at FROM messages
                 WHERE role = 'user'
                 ORDER BY created_at DESC LIMIT ?1",
            )?;
            let rows = stmt.query_map(rusqlite::params![limit as i64], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })?;
            Ok(rows.collect::<Result<Vec<_>, _>>()?)
        }
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
            .search_embeddings(entity_kind::FACT, &[1.0, 0.0], 10, "m")
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
            .search_embeddings(entity_kind::FACT, &[1.0, 0.0], 1, "m")
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].0.entity_id, "f1");
    }

    #[test]
    fn search_embeddings_filtered_by_fact_subject() {
        let db = db();
        let user = db
            .insert_fact("user", "likes", "Rust", "user", 0.9, &[])
            .unwrap();
        let other = db
            .insert_fact("alice", "likes", "Go", "user", 0.9, &[])
            .unwrap();
        db.save_embedding(entity_kind::FACT, &user.id, "m", &[1.0, 0.0], "user rust")
            .unwrap();
        db.save_embedding(entity_kind::FACT, &other.id, "m", &[1.0, 0.0], "alice go")
            .unwrap();
        let hits = db
            .search_embeddings_filtered(
                entity_kind::FACT,
                &[1.0, 0.0],
                10,
                "m",
                Some("user"),
                None,
            )
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].0.entity_id, user.id);
    }

    #[test]
    fn search_embeddings_filtered_excludes_session_episodes() {
        let db = db();
        let current = db.create_session("cur", "").unwrap();
        let other = db.create_session("oth", "").unwrap();
        let cur_ep = db.add_episode(&current.id, "current summary").unwrap();
        let oth_ep = db.add_episode(&other.id, "other summary").unwrap();
        db.save_embedding(entity_kind::EPISODE, &cur_ep, "m", &[1.0, 0.0], "current")
            .unwrap();
        db.save_embedding(entity_kind::EPISODE, &oth_ep, "m", &[1.0, 0.0], "other")
            .unwrap();
        let hits = db
            .search_embeddings_filtered(
                entity_kind::EPISODE,
                &[1.0, 0.0],
                10,
                "m",
                None,
                Some(&current.id),
            )
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].0.entity_id, oth_ep);
    }

    #[test]
    fn search_embeddings_filters_by_model() {
        let db = db();
        db.save_embedding(entity_kind::FACT, "f1", "old-m", &[1.0, 0.0], "a")
            .unwrap();
        db.save_embedding(entity_kind::FACT, "f2", "new-m", &[1.0, 0.0], "b")
            .unwrap();
        let hits = db
            .search_embeddings(entity_kind::FACT, &[1.0, 0.0], 10, "new-m")
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].0.entity_id, "f2");
        assert!(
            db.search_embeddings(entity_kind::FACT, &[1.0, 0.0], 10, "")
                .unwrap()
                .is_empty(),
            "empty model must fail-closed"
        );
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
        let missing = db.missing_embedding_ids(entity_kind::FACT, "m").unwrap();
        assert_eq!(missing.len(), 1);
        // Covered under old model still missing for the new one (P2-13).
        let missing_new = db.missing_embedding_ids(entity_kind::FACT, "other").unwrap();
        assert_eq!(missing_new.len(), 2);
    }

    #[test]
    fn missing_embedding_ids_episodes() {
        let db = db();
        let session = db.create_session("t", "").unwrap();
        let msg = db
            .add_message(&session.id, "user", "hello world", Some("text"), None)
            .unwrap();
        let missing = db.missing_embedding_ids(entity_kind::EPISODE, "m").unwrap();
        assert_eq!(missing.len(), 1);
        assert!(missing.contains(&msg.id));
        db.save_embedding(entity_kind::EPISODE, &msg.id, "m", &[1.0], "hello world")
            .unwrap();
        let missing = db.missing_embedding_ids(entity_kind::EPISODE, "m").unwrap();
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
            .missing_embedding_ids_limited(entity_kind::EPISODE, "m", 2)
            .unwrap();
        assert_eq!(missing.len(), 2);
        assert!(
            missing.contains(&ep_a) && missing.contains(&ep_b),
            "summaries must fill the cap before raw user messages, got {:?}",
            missing
        );

        let missing3 = db
            .missing_embedding_ids_limited(entity_kind::EPISODE, "m", 3)
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
    fn search_episodes_by_keywords_excludes_current_session() {
        let db = db();
        let current = db.create_session("current", "").unwrap();
        let past = db.create_session("past", "").unwrap();
        db.add_message(
            &current.id,
            "user",
            "I discussed the dark theme in this session",
            Some("text"),
            None,
        )
        .unwrap();
        db.add_message(
            &past.id,
            "user",
            "I discussed the dark theme last week",
            Some("text"),
            None,
        )
        .unwrap();
        let hits = db
            .search_episodes_by_keywords_excluding(&["dark", "theme"], 5, Some(&current.id))
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert!(hits[0].contains("last week"));
        assert!(!hits.iter().any(|h| h.contains("this session")));
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
        let missing = db.missing_embedding_ids(entity_kind::EPISODE, "m").unwrap();
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
        let missing = db.missing_embedding_ids(entity_kind::EPISODE, "m").unwrap();
        assert!(!missing.contains(&ep));
        assert_eq!(db.prune_orphaned_embeddings().unwrap(), 0);
    }

    #[test]
    fn search_episodes_by_topics_via_fts() {
        let db = db();
        let session = db.create_session("t", "").unwrap();
        let id = haven_common::types::new_id("msg");
        db.add_episode_structured(
            &session.id,
            "talked about monitors",
            &id,
            &["hardware"],
            &["Dell"],
        )
        .unwrap();
        let hits = db
            .search_episodes_by_keywords(&["hardware"], 5)
            .unwrap();
        assert!(
            hits.iter().any(|h| h.contains("monitors")),
            "topic tag must be FTS-visible; got {hits:?}"
        );
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
