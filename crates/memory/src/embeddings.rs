use crate::db::Database;
use rusqlite::OptionalExtension;

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

/// Stable keyword candidate for an episodic memory item. The public legacy
/// search facade still returns only text; typed recall uses this identity so
/// equal summaries from different sessions are not collapsed together.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EpisodeKeywordHit {
    pub entity_id: String,
    pub text: String,
}

type EpisodeSearchRow = (String, String, String, String);

/// Memory domain constants used as `memory_embeddings.entity_type`.
///
/// These are **embedding-domain aliases** for the graph tables. Unified FTS
/// uses a separate vocabulary (`fts_kind`) — never mix the two in filters.
pub mod entity_kind {
    /// Embedding domain for `memory_edges` SPO rows (alias kept as `fact`).
    pub const FACT: &str = "fact";
    /// Embedding domain for `memory_items` rows (alias kept as `episode`).
    pub const EPISODE: &str = "episode";
}

/// `memory_fts.entity_type` vocabulary (X1 unified FTS). Maps to
/// [`entity_kind`] as: `EDGE` ↔ `FACT`, `ITEM` ↔ `EPISODE`.
pub mod fts_kind {
    pub const EDGE: &str = "edge";
    pub const ITEM: &str = "item";
}

/// Max unembedded facts returned per missing-ids scan (recent first).
pub const FACT_EMBED_BACKLOG_LIMIT: usize = 128;

/// Max unembedded memory items returned per missing-ids scan. Prevents a
/// single maintenance / hot-path embed pass from exploding after enabling or
/// switching the embedding model on a large history.
pub const EPISODE_EMBED_BACKLOG_LIMIT: usize = 64;

/// Cap on embeddings scored per brute-force search when no tighter domain
/// filter applies (P1-4). Prefer newest rows; above [`ANN_ACTIVATE_MIN`] the
/// LSH probe path replaces unbounded brute force (M5).
pub const EMBEDDING_SEARCH_SCAN_CAP: usize = 256;

/// Activate pure-Rust LSH candidate probing once a (entity_type, model)
/// partition reaches this many rows (M5). Below the threshold the existing
/// newest-first scan cap stays in force.
pub const ANN_ACTIVATE_MIN: usize = 4096;

/// Bits in the LSH signature (M5). 16 bits → 65 536 buckets; Hamming-1
/// probes stay cheap while still collapsing a 10k+ partition.
const LSH_BITS: u32 = 16;

/// Max candidates pulled from LSH buckets before exact cosine re-rank (M5).
const LSH_PROBE_CANDIDATE_CAP: usize = 1024;

/// Serialize an f32 vector as a little-endian byte blob for SQLite storage.
pub fn encode_vector(v: &[f32]) -> Vec<u8> {
    v.iter().flat_map(|f| f.to_le_bytes()).collect()
}

/// Deserialize a stored little-endian f32 blob back into a vector.
pub fn decode_vector(blob: &[u8]) -> Vec<f32> {
    blob.as_chunks::<4>()
        .0
        .iter()
        .copied()
        .map(f32::from_le_bytes)
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

/// Deterministic unit-ish component for LSH hyperplane `bit` at dimension `i`
/// (M5). No stored plane table — same (bit, i) always yields the same sign
/// contribution so buckets stay stable across process restarts.
fn lsh_plane_component(bit: u32, i: usize) -> f32 {
    let mut x = (bit as u64)
        .wrapping_mul(0x9E37_79B9_7F4A_7C15)
        .wrapping_add((i as u64).wrapping_mul(0xBF58_476D_1CE4_E5B9));
    x ^= x >> 30;
    x = x.wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x ^= x >> 27;
    // Map to [-1, 1]
    let u = (x >> 40) as u32;
    (u as f32 / (1u32 << 24) as f32) * 2.0 - 1.0
}

/// Random-projection LSH bucket for `vec` (M5). Empty vectors hash to 0.
pub fn lsh_bucket(vec: &[f32]) -> i64 {
    if vec.is_empty() {
        return 0;
    }
    let mut bucket: u64 = 0;
    for bit in 0..LSH_BITS {
        let mut dot = 0.0f32;
        for (i, &v) in vec.iter().enumerate() {
            dot += v * lsh_plane_component(bit, i);
        }
        if dot >= 0.0 {
            bucket |= 1u64 << bit;
        }
    }
    bucket as i64
}

/// Buckets at Hamming distance ≤ 1 from `bucket` (self included) for LSH
/// probing (M5).
fn lsh_probe_buckets(bucket: i64) -> Vec<i64> {
    let base = bucket as u64;
    let mut out = Vec::with_capacity(1 + LSH_BITS as usize);
    out.push(bucket);
    for bit in 0..LSH_BITS {
        out.push((base ^ (1u64 << bit)) as i64);
    }
    out
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

/// SQL predicate for a live embedding owner. The embedding table is
/// intentionally polymorphic, so SQLite cannot express this as a foreign key.
/// Unknown legacy domains remain readable for compatibility; the two current
/// domains must have a live graph/item row.
fn live_owner_filter(alias: &str) -> String {
    let entity_type = if alias.is_empty() {
        "entity_type".to_string()
    } else {
        format!("{alias}.entity_type")
    };
    let entity_id = if alias.is_empty() {
        "entity_id".to_string()
    } else {
        format!("{alias}.entity_id")
    };
    format!(
        "({entity_type} NOT IN ('fact', 'episode')
          OR ({entity_type} = 'fact' AND EXISTS (
              SELECT 1 FROM memory_edges WHERE id = {entity_id}
          ))
          OR ({entity_type} = 'episode' AND EXISTS (
              SELECT 1 FROM memory_items WHERE id = {entity_id}
          )))"
    )
}

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
        // The provider response may contain a canonical alias or versioned
        // model name. A missing-id scan binds each entity to the configured
        // index identity before the network request; use that identity for
        // persistence and never let the response rename the index partition.
        let pending_model = self.pending_embedding_model(entity_type, entity_id);
        if pending_model
            .as_deref()
            .is_some_and(|expected_model| expected_model != model)
        {
            // A newer embedding pass claimed this entity under another model
            // while an older provider request was still in flight. The old
            // response must not overwrite the newer claim or be relabeled as
            // the new vector space.
            return Ok(());
        }
        let stored_model = pending_model.as_deref().unwrap_or(model);
        let conn = self.conn();
        // A pending batch is the only production write path. Recheck the
        // owner and its current surface text under the same write transaction
        // that persists the vector: deletion or SPO/content update while the
        // provider request was in flight becomes a harmless no-op instead of
        // resurrecting an orphan/stale embedding.
        let owner_recheck = pending_model.is_some();
        conn.execute_batch("BEGIN IMMEDIATE")?;
        let result = (|| -> anyhow::Result<bool> {
            if owner_recheck {
                let current_text: Option<String> = match entity_type {
                    entity_kind::FACT => conn.query_row(
                        "SELECT subject || ' ' || predicate || ' ' || object
                         FROM memory_edges WHERE id = ?1",
                        rusqlite::params![entity_id],
                        |row| row.get(0),
                    ),
                    entity_kind::EPISODE => conn.query_row(
                        "SELECT content FROM memory_items WHERE id = ?1",
                        rusqlite::params![entity_id],
                        |row| row.get(0),
                    ),
                    _ => Ok(text.to_string()),
                }
                .optional()?;
                if current_text.as_deref() != Some(text) {
                    return Ok(false);
                }
            }

            let bucket = lsh_bucket(vector);
            conn.execute(
                "INSERT INTO memory_embeddings (entity_type, entity_id, model, vector, text, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)
                 ON CONFLICT(entity_type, entity_id, model)
                 DO UPDATE SET vector = excluded.vector, text = excluded.text, updated_at = excluded.updated_at",
                rusqlite::params![entity_type, entity_id, stored_model, blob, text, now],
            )?;
            conn.execute(
                "INSERT INTO embedding_lsh (entity_type, entity_id, model, bucket)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(entity_type, entity_id, model)
                 DO UPDATE SET bucket = excluded.bucket",
                rusqlite::params![entity_type, entity_id, stored_model, bucket],
            )?;
            Ok(true)
        })();
        let saved = match result {
            Ok(saved) => {
                conn.execute_batch("COMMIT")?;
                saved
            }
            Err(error) => {
                let _ = conn.execute_batch("ROLLBACK");
                return Err(error);
            }
        };
        drop(conn);
        if pending_model.is_some() {
            self.clear_pending_embedding_model(entity_type, entity_id);
        }
        if saved {
            self.cache_invalidate_embeddings(entity_type);
        }
        Ok(())
    }

    /// Count embeddings for one domain + model (M5 activation gate).
    pub fn count_embeddings_for_model(
        &self,
        entity_type: &str,
        model: &str,
    ) -> anyhow::Result<usize> {
        if model.is_empty() {
            return Ok(0);
        }
        let conn = self.conn();
        let n: i64 = conn.query_row(
            &format!(
                "SELECT COUNT(*) FROM memory_embeddings
                 WHERE entity_type = ?1 AND model = ?2 AND {}",
                live_owner_filter("")
            ),
            rusqlite::params![entity_type, model],
            |r| r.get(0),
        )?;
        Ok(n as usize)
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
                 WHERE entity_type = ?1 AND entity_id = ?2 AND model = ?3
                   AND {}",
                live_owner_filter("")
            ))?;
            let mut rows = stmt.query(rusqlite::params![entity_type, entity_id, model])?;
            return match rows.next()? {
                Some(row) => Ok(Some(row_to_embedded(row)?)),
                None => Ok(None),
            };
        }
        let mut stmt = conn.prepare(&format!(
            "SELECT {EMBED_COLS} FROM memory_embeddings
             WHERE entity_type = ?1 AND entity_id = ?2 AND {}",
            live_owner_filter("")
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
             AND {} ORDER BY updated_at DESC, entity_id ASC, model ASC",
            live_owner_filter("")
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
        let ids = match entity_type {
            entity_kind::FACT => {
                let mut out = Vec::new();
                if model_filter {
                    let mut stmt = conn.prepare(
                        "SELECT id FROM memory_edges
                         WHERE id NOT IN (
                             SELECT entity_id FROM memory_embeddings
                             WHERE entity_type = ?1 AND model = ?2
                         )
                         ORDER BY COALESCE(last_seen_at, created_at) DESC
                         LIMIT ?3",
                    )?;
                    for row in stmt
                        .query_map(rusqlite::params![entity_type, model, limit as i64], |r| {
                            r.get::<_, String>(0)
                        })?
                    {
                        out.push(row?);
                    }
                } else {
                    let mut stmt = conn.prepare(
                        "SELECT id FROM memory_edges
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
                out
            }
            entity_kind::EPISODE => {
                let mut out: Vec<String> = Vec::new();
                if model_filter {
                    let mut ep_stmt = conn.prepare(
                        "SELECT id FROM memory_items
                         WHERE id NOT IN (
                             SELECT entity_id FROM memory_embeddings
                             WHERE entity_type = ?1 AND model = ?2
                         )
                         ORDER BY created_at DESC
                         LIMIT ?3",
                    )?;
                    for row in ep_stmt
                        .query_map(rusqlite::params![entity_type, model, limit as i64], |r| {
                            r.get::<_, String>(0)
                        })?
                    {
                        out.push(row?);
                    }
                } else {
                    let mut ep_stmt = conn.prepare(
                        "SELECT id FROM memory_items
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
                out
            }
            _ => Vec::new(),
        };
        drop(conn);
        for id in &ids {
            self.register_pending_embedding_model(entity_type, id, model);
        }
        Ok(ids)
    }

    /// Embeddable surface text for a fact: `subject predicate object`.
    pub fn fact_text_by_id(&self, fact_id: &str) -> anyhow::Result<Option<String>> {
        let conn = self.conn();
        let text = match conn.query_row(
            "SELECT subject || ' ' || predicate || ' ' || object FROM memory_edges WHERE id = ?1",
            rusqlite::params![fact_id],
            |r| r.get(0),
        ) {
            Ok(t) => Some(t),
            Err(rusqlite::Error::QueryReturnedNoRows) => None,
            Err(e) => return Err(e.into()),
        };
        Ok(text)
    }

    /// Source text for an episode-domain entity: `memory_items.content`.
    pub fn episode_text(&self, entity_id: &str) -> anyhow::Result<Option<String>> {
        let conn = self.conn();
        let content = match conn.query_row(
            "SELECT content FROM memory_items WHERE id = ?1",
            rusqlite::params![entity_id],
            |r| r.get::<_, String>(0),
        ) {
            Ok(c) => Some(c),
            Err(rusqlite::Error::QueryReturnedNoRows) => None,
            Err(e) => return Err(e.into()),
        };
        Ok(content)
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
    /// Below [`ANN_ACTIVATE_MIN`] the candidate set is bounded by
    /// `max(limit * 4, EMBEDDING_SEARCH_SCAN_CAP)` newest matching rows.
    /// At/above that size, LSH bucket probing (M5) replaces the newest-only
    /// scan so older high-similarity rows are still reachable.
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
        let total = self.count_embeddings_for_model(entity_type, model)?;
        let use_ann = total >= ANN_ACTIVATE_MIN
            && self.embedding_lsh_ready_for_ann(entity_type, model, total)?;
        let candidates = if use_ann {
            let probe_cap = (limit.saturating_mul(8)).max(LSH_PROBE_CANDIDATE_CAP);
            self.list_embeddings_via_lsh(
                entity_type,
                model,
                query_vec,
                probe_cap,
                fact_subject,
                exclude_session_id,
            )?
        } else {
            let scan_cap = (limit.saturating_mul(4)).max(EMBEDDING_SEARCH_SCAN_CAP);
            self.list_embeddings_for_search(
                entity_type,
                model,
                scan_cap,
                fact_subject,
                exclude_session_id,
            )?
        };
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

    /// True when LSH side-table coverage matches the embedding partition so
    /// ANN probing will not silently hide unbucketed rows (M5).
    pub fn embedding_lsh_ready_for_ann(
        &self,
        entity_type: &str,
        model: &str,
        embed_count: usize,
    ) -> anyhow::Result<bool> {
        if model.is_empty() || embed_count < ANN_ACTIVATE_MIN {
            return Ok(false);
        }
        let conn = self.conn();
        let lsh_count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM embedding_lsh WHERE entity_type = ?1 AND model = ?2",
            rusqlite::params![entity_type, model],
            |r| r.get(0),
        )?;
        Ok(lsh_count as usize >= embed_count)
    }

    /// True when any embedding for `model` lacks an LSH bucket (M5 rebuild gate).
    pub fn embedding_lsh_lagging(&self, model: &str) -> anyhow::Result<bool> {
        if model.is_empty() {
            return Ok(false);
        }
        let conn = self.conn();
        let embeds: i64 = conn.query_row(
            "SELECT COUNT(*) FROM memory_embeddings WHERE model = ?1",
            rusqlite::params![model],
            |r| r.get(0),
        )?;
        let lsh: i64 = conn.query_row(
            "SELECT COUNT(*) FROM embedding_lsh WHERE model = ?1",
            rusqlite::params![model],
            |r| r.get(0),
        )?;
        Ok(lsh < embeds)
    }

    /// LSH candidate fetch for large partitions (M5). Probes the query bucket
    /// and Hamming-1 neighbors with the same SQL domain filters as the brute
    /// path; falls back to newest scan when the probe is empty.
    fn list_embeddings_via_lsh(
        &self,
        entity_type: &str,
        model: &str,
        query_vec: &[f32],
        probe_cap: usize,
        fact_subject: Option<&str>,
        exclude_session_id: Option<&str>,
    ) -> anyhow::Result<Vec<EmbeddedText>> {
        let buckets = lsh_probe_buckets(lsh_bucket(query_vec));
        let placeholders = buckets.iter().map(|_| "?").collect::<Vec<_>>().join(",");

        let mut bind: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
        bind.push(Box::new(entity_type.to_string()));
        bind.push(Box::new(model.to_string()));
        for b in &buckets {
            bind.push(Box::new(*b));
        }

        let mut sql = String::from(
            "SELECT e.entity_type, e.entity_id, e.model, e.vector, e.text,
                    e.created_at, e.updated_at
             FROM memory_embeddings e
             INNER JOIN embedding_lsh l
               ON l.entity_type = e.entity_type
              AND l.entity_id = e.entity_id
              AND l.model = e.model ",
        );
        if entity_type == entity_kind::FACT && fact_subject.is_some_and(|s| !s.is_empty()) {
            sql.push_str("INNER JOIN memory_edges f ON f.id = e.entity_id ");
        }
        if entity_type == entity_kind::EPISODE {
            sql.push_str("INNER JOIN memory_items i ON i.id = e.entity_id ");
        }
        sql.push_str(&format!(
            "WHERE e.entity_type = ? AND e.model = ? AND l.bucket IN ({placeholders})
             AND {} ",
            live_owner_filter("e")
        ));
        if entity_type == entity_kind::FACT
            && let Some(subject) = fact_subject.filter(|s| !s.is_empty())
        {
            sql.push_str("AND f.subject = ? ");
            bind.push(Box::new(subject.to_string()));
        }
        if entity_type == entity_kind::EPISODE
            && let Some(sid) = exclude_session_id.filter(|s| !s.is_empty())
        {
            sql.push_str(
                "AND e.entity_id NOT IN (
                     SELECT id FROM memory_items WHERE session_id = ?
                 ) ",
            );
            bind.push(Box::new(sid.to_string()));
        }
        sql.push_str("ORDER BY e.updated_at DESC LIMIT ?");
        bind.push(Box::new(probe_cap as i64));

        let out = {
            let conn = self.conn();
            let mut stmt = conn.prepare(&sql)?;
            let params: Vec<&dyn rusqlite::ToSql> = bind.iter().map(|b| b.as_ref()).collect();
            let rows = stmt.query_map(params.as_slice(), row_to_embedded)?;
            let mut out = Vec::new();
            for row in rows {
                out.push(row?);
            }
            out
        };
        if out.is_empty() {
            let scan_cap = probe_cap.min(EMBEDDING_SEARCH_SCAN_CAP.saturating_mul(4));
            return self.list_embeddings_for_search(
                entity_type,
                model,
                scan_cap,
                fact_subject,
                exclude_session_id,
            );
        }
        Ok(out)
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
                     INNER JOIN memory_edges f ON f.id = e.entity_id
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
                     INNER JOIN memory_items i ON i.id = e.entity_id
                     WHERE e.entity_type = ?1 AND e.model = ?2
                       AND i.session_id != ?3
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
                       AND {}
                     ORDER BY updated_at DESC
                     LIMIT ?3",
                    live_owner_filter("")
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
    /// Memory items use unified `memory_fts` (trigram) when available;
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
        Ok(self
            .search_episodes_by_keywords_typed(terms, limit, exclude_session_id)?
            .into_iter()
            .map(|hit| hit.text)
            .collect())
    }

    /// Typed keyword episode search. Unlike the legacy text-only facade, each
    /// result carries the owning `memory_items.id`, so hybrid recall can
    /// deduplicate the same episode across keyword and vector candidates even
    /// when two summaries have identical text.
    pub(crate) fn search_episodes_by_keywords_typed(
        &self,
        terms: &[&str],
        limit: usize,
        exclude_session_id: Option<&str>,
    ) -> anyhow::Result<Vec<EpisodeKeywordHit>> {
        let terms: Vec<&str> = terms.iter().filter(|t| !t.is_empty()).copied().collect();
        if terms.is_empty() || limit == 0 {
            return Ok(Vec::new());
        }
        let lower_terms: Vec<String> = terms.iter().map(|t| t.to_lowercase()).collect();
        let mut scored: Vec<(usize, String, String, String)> = Vec::new();
        let mut seen = std::collections::HashSet::new();

        // P2-10: prefer FTS for compaction summaries (+ topics/entities).
        if let Ok(Some(fts_hits)) =
            self.search_episode_summaries_fts(&terms, exclude_session_id, limit.saturating_mul(4))
        {
            for (id, display, haystack, created) in fts_hits {
                Self::score_episode_candidate_haystack(
                    &id,
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
            for (id, display, haystack, created) in
                self.list_recent_episode_rows(exclude_session_id, 1000)?
            {
                Self::score_episode_candidate_haystack(
                    &id,
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
            for (id, display, haystack, created) in
                self.list_recent_episode_rows(exclude_session_id, 1000)?
            {
                let hay = haystack.to_lowercase();
                if short_lower.iter().any(|p| hay.contains(p)) {
                    Self::score_episode_candidate_haystack(
                        &id,
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

        scored.sort_by(|a, b| {
            b.0.cmp(&a.0)
                .then_with(|| b.3.cmp(&a.3)) // newer created_at first
                .then_with(|| a.1.cmp(&b.1))
        });
        scored.truncate(limit);
        Ok(scored
            .into_iter()
            .map(|(_, id, text, _)| EpisodeKeywordHit {
                entity_id: id,
                text,
            })
            .collect())
    }

    fn score_episode_candidate_haystack(
        entity_id: &str,
        display: &str,
        haystack: &str,
        created: &str,
        lower_terms: &[String],
        scored: &mut Vec<(usize, String, String, String)>,
        seen: &mut std::collections::HashSet<String>,
    ) {
        if !seen.insert(entity_id.to_string()) {
            return;
        }
        let tl = haystack.to_lowercase();
        let hits = lower_terms
            .iter()
            .filter(|term| tl.contains(term.as_str()))
            .count();
        if hits > 0 {
            scored.push((
                hits,
                entity_id.to_string(),
                display.to_string(),
                created.to_string(),
            ));
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
    /// Rows are `(entity_id, display_summary, search_haystack, created_at)`.
    fn search_episode_summaries_fts(
        &self,
        terms: &[&str],
        exclude_session_id: Option<&str>,
        limit: usize,
    ) -> anyhow::Result<Option<Vec<EpisodeSearchRow>>> {
        if limit == 0 || terms.is_empty() {
            return Ok(Some(Vec::new()));
        }
        let match_expr = Self::build_episode_fts_query(terms);
        let map_row = |r: &rusqlite::Row| -> rusqlite::Result<EpisodeSearchRow> {
            let id: String = r.get(0)?;
            let content: String = r.get(1)?;
            let topics: String = r.get(2)?;
            let entities: String = r.get(3)?;
            let created: String = r.get(4)?;
            let haystack = format!("{content} {topics} {entities}");
            Ok((id, content, haystack, created))
        };
        let item = fts_kind::ITEM;
        let conn = self.conn();
        let result = if let Some(sid) = exclude_session_id {
            let sql = format!(
                "SELECT e.id, e.content, e.topics, e.entities, e.created_at
                 FROM memory_fts
                 JOIN memory_items e ON e.id = memory_fts.entity_id
                 WHERE memory_fts.entity_type = '{item}'
                   AND memory_fts MATCH ?1 AND e.session_id != ?2
                 ORDER BY bm25(memory_fts), e.created_at DESC
                 LIMIT ?3"
            );
            let mut stmt = conn.prepare(&sql);
            match stmt {
                Ok(ref mut s) => s
                    .query_map(rusqlite::params![match_expr, sid, limit as i64], map_row)
                    .and_then(|rows| rows.collect::<Result<Vec<_>, _>>()),
                Err(e) => Err(e),
            }
        } else {
            let sql = format!(
                "SELECT e.id, e.content, e.topics, e.entities, e.created_at
                 FROM memory_fts
                 JOIN memory_items e ON e.id = memory_fts.entity_id
                 WHERE memory_fts.entity_type = '{item}'
                   AND memory_fts MATCH ?1
                 ORDER BY bm25(memory_fts), e.created_at DESC
                 LIMIT ?2"
            );
            let mut stmt = conn.prepare(&sql);
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

    /// `(entity_id, display_summary, search_haystack, created_at)` — haystack includes
    /// topics/entities JSON so structured tags are keyword-visible (P2-10).
    fn list_recent_episode_rows(
        &self,
        exclude_session_id: Option<&str>,
        limit: usize,
    ) -> anyhow::Result<Vec<EpisodeSearchRow>> {
        let conn = self.conn();
        let map_row = |r: &rusqlite::Row| -> rusqlite::Result<EpisodeSearchRow> {
            let id: String = r.get(0)?;
            let content: String = r.get(1)?;
            let topics: String = r.get(2)?;
            let entities: String = r.get(3)?;
            let created: String = r.get(4)?;
            let haystack = format!("{content} {topics} {entities}");
            Ok((id, content, haystack, created))
        };
        if let Some(sid) = exclude_session_id {
            let mut stmt = conn.prepare(
                "SELECT id, content, topics, entities, created_at FROM memory_items
                 WHERE session_id != ?1
                 ORDER BY created_at DESC LIMIT ?2",
            )?;
            let rows = stmt.query_map(rusqlite::params![sid, limit as i64], map_row)?;
            Ok(rows.collect::<Result<Vec<_>, _>>()?)
        } else {
            let mut stmt = conn.prepare(
                "SELECT id, content, topics, entities, created_at FROM memory_items
                 ORDER BY created_at DESC LIMIT ?1",
            )?;
            let rows = stmt.query_map(rusqlite::params![limit as i64], map_row)?;
            Ok(rows.collect::<Result<Vec<_>, _>>()?)
        }
    }

    /// Distinct embedding model names currently in the vector index.
    pub fn list_embedding_models(&self) -> anyhow::Result<Vec<String>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!(
            "SELECT DISTINCT model FROM memory_embeddings e WHERE {}",
            live_owner_filter("e")
        ))?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Drop every stored embedding. Used when the configured embedding model
    /// changes: vectors from a different model are not comparable (dimension
    /// mismatch makes cosine similarity degenerate), so the index is rebuilt
    /// from scratch on the next embedding pass.
    pub fn clear_embeddings(&self) -> anyhow::Result<u64> {
        let conn = self.conn();
        conn.execute_batch("BEGIN IMMEDIATE")?;
        let result = (|| -> anyhow::Result<u64> {
            let deleted = conn.execute("DELETE FROM memory_embeddings", [])? as u64;
            conn.execute("DELETE FROM embedding_lsh", [])?;
            Ok(deleted)
        })();
        let deleted = match result {
            Ok(deleted) => {
                conn.execute_batch("COMMIT")?;
                deleted
            }
            Err(error) => {
                let _ = conn.execute_batch("ROLLBACK");
                return Err(error);
            }
        };
        drop(conn);
        self.clear_pending_embedding_models();
        if deleted > 0 {
            self.cache_invalidate_embeddings(entity_kind::FACT);
            self.cache_invalidate_embeddings(entity_kind::EPISODE);
        }
        Ok(deleted)
    }

    /// Remove embeddings whose owning entity no longer exists (edges/items
    /// deleted). Keeps the index from growing unbounded around pruned memory.
    pub fn prune_orphaned_embeddings(&self) -> anyhow::Result<u64> {
        let conn = self.conn();
        conn.execute_batch("BEGIN IMMEDIATE")?;
        let result = (|| -> anyhow::Result<(u64, u64)> {
            let deleted = conn.execute(
                "DELETE FROM memory_embeddings WHERE
                    (entity_type = 'fact' AND entity_id NOT IN (SELECT id FROM memory_edges))
                 OR (entity_type = 'episode'
                     AND entity_id NOT IN (SELECT id FROM memory_items))",
                [],
            )? as u64;
            let deleted_lsh = conn.execute(
                "DELETE FROM embedding_lsh WHERE
                    (entity_type, entity_id, model) NOT IN (
                        SELECT entity_type, entity_id, model FROM memory_embeddings
                    )",
                [],
            )? as u64;
            Ok((deleted, deleted_lsh))
        })();
        let (deleted, deleted_lsh) = match result {
            Ok(counts) => {
                conn.execute_batch("COMMIT")?;
                counts
            }
            Err(error) => {
                let _ = conn.execute_batch("ROLLBACK");
                return Err(error);
            }
        };
        drop(conn);
        if deleted > 0 {
            self.cache_invalidate_embeddings(entity_kind::FACT);
            self.cache_invalidate_embeddings(entity_kind::EPISODE);
        }
        if deleted > 0 || deleted_lsh > 0 {
            self.clear_pending_embedding_models();
        }
        Ok(deleted)
    }

    /// Rebuild LSH buckets for every embedding of `model` (M5). Best-effort
    /// maintenance hook after model switches / partial index lag. Runs in one
    /// transaction so a crash cannot leave a half-built side table.
    pub fn rebuild_embedding_lsh(&self, model: &str) -> anyhow::Result<u64> {
        if model.is_empty() {
            return Ok(0);
        }
        let rows: Vec<(String, String, Vec<f32>)> = {
            let conn = self.conn();
            let sql = format!(
                "SELECT entity_type, entity_id, vector FROM memory_embeddings e
                 WHERE model = ?1 AND {}",
                live_owner_filter("e")
            );
            let mut stmt = conn.prepare(&sql)?;
            let mapped = stmt.query_map(rusqlite::params![model], |row| {
                let entity_type: String = row.get(0)?;
                let entity_id: String = row.get(1)?;
                let blob: Vec<u8> = row.get(2)?;
                Ok((entity_type, entity_id, decode_vector(&blob)))
            })?;
            mapped.collect::<Result<Vec<_>, _>>()?
        };
        let conn = self.conn();
        conn.execute_batch("BEGIN IMMEDIATE")?;
        let result = (|| -> anyhow::Result<u64> {
            let mut n = 0u64;
            for (entity_type, entity_id, vector) in &rows {
                let bucket = lsh_bucket(vector);
                conn.execute(
                    "INSERT INTO embedding_lsh (entity_type, entity_id, model, bucket)
                     VALUES (?1, ?2, ?3, ?4)
                     ON CONFLICT(entity_type, entity_id, model)
                     DO UPDATE SET bucket = excluded.bucket",
                    rusqlite::params![entity_type, entity_id, model, bucket],
                )?;
                n += 1;
            }
            Ok(n)
        })();
        match result {
            Ok(n) => {
                conn.execute_batch("COMMIT")?;
                Ok(n)
            }
            Err(e) => {
                let _ = conn.execute_batch("ROLLBACK");
                Err(e)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Database;

    fn db() -> Database {
        Database::open_in_memory().unwrap()
    }

    fn insert_fact_with_id(db: &Database, id: &str) {
        let conn = db.conn();
        conn.execute(
            "INSERT INTO memory_edges (id, subject, predicate, object, source, confidence, created_at)
             VALUES (?1, 'user', 'test', ?1, 'inferred', 1.0, '2026-01-01T00:00:00Z')",
            rusqlite::params![id],
        )
        .unwrap();
    }

    fn insert_episode_with_id(db: &Database, id: &str) {
        let session = db.create_session("embedding-test", "").unwrap();
        db.add_episode_with_id(&session.id, "test episode", id)
            .unwrap();
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
        insert_fact_with_id(&db, "f1");
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
        insert_fact_with_id(&db, "f1");
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
        insert_fact_with_id(&db, "f1");
        insert_fact_with_id(&db, "f2");
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
        insert_fact_with_id(&db, "f1");
        insert_episode_with_id(&db, "e1");
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
            .search_embeddings_filtered(entity_kind::FACT, &[1.0, 0.0], 10, "m", Some("user"), None)
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
        insert_fact_with_id(&db, "f1");
        insert_fact_with_id(&db, "f2");
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
        let missing_new = db
            .missing_embedding_ids(entity_kind::FACT, "other")
            .unwrap();
        assert_eq!(missing_new.len(), 2);
    }

    #[test]
    fn pending_embedding_save_does_not_resurrect_stale_fact() {
        let db = db();
        let fact = db
            .insert_fact("user", "likes", "Rust", "inferred", 0.9, &[])
            .unwrap();
        let missing = db
            .missing_embedding_ids(entity_kind::FACT, "model-a")
            .unwrap();
        assert_eq!(missing, vec![fact.id.clone()]);

        {
            let conn = db.conn();
            conn.execute(
                "UPDATE memory_edges SET object = 'Go' WHERE id = ?1",
                rusqlite::params![fact.id],
            )
            .unwrap();
        }

        db.save_embedding(
            entity_kind::FACT,
            &fact.id,
            "model-a",
            &[1.0, 0.0],
            "user likes Rust",
        )
        .unwrap();
        assert!(
            db.get_embedding_for_model(entity_kind::FACT, &fact.id, Some("model-a"))
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn stale_embedding_model_cannot_claim_new_model_pending_row() {
        let db = db();
        let fact = db
            .insert_fact("user", "likes", "Rust", "inferred", 0.9, &[])
            .unwrap();
        assert_eq!(
            db.missing_embedding_ids(entity_kind::FACT, "model-a")
                .unwrap(),
            vec![fact.id.clone()]
        );
        assert_eq!(
            db.missing_embedding_ids(entity_kind::FACT, "model-b")
                .unwrap(),
            vec![fact.id.clone()]
        );

        db.save_embedding(
            entity_kind::FACT,
            &fact.id,
            "model-a",
            &[1.0, 0.0],
            "user likes Rust",
        )
        .unwrap();
        assert!(
            db.get_embedding_for_model(entity_kind::FACT, &fact.id, Some("model-a"))
                .unwrap()
                .is_none()
        );

        db.save_embedding(
            entity_kind::FACT,
            &fact.id,
            "model-b",
            &[0.0, 1.0],
            "user likes Rust",
        )
        .unwrap();
        assert_eq!(
            db.get_embedding_for_model(entity_kind::FACT, &fact.id, Some("model-b"))
                .unwrap()
                .unwrap()
                .vector,
            vec![0.0, 1.0]
        );
    }

    #[test]
    fn missing_embedding_ids_episodes() {
        let db = db();
        let session = db.create_session("t", "").unwrap();
        let ep = db.add_episode(&session.id, "hello world").unwrap();
        let missing = db.missing_embedding_ids(entity_kind::EPISODE, "m").unwrap();
        assert_eq!(missing.len(), 1);
        assert!(missing.contains(&ep));
        db.save_embedding(entity_kind::EPISODE, &ep, "m", &[1.0], "hello world")
            .unwrap();
        let missing = db.missing_embedding_ids(entity_kind::EPISODE, "m").unwrap();
        assert_eq!(missing.len(), 0);
    }

    #[test]
    fn missing_embedding_ids_episodes_respects_limit_and_prefers_summaries() {
        let db = db();
        let session = db.create_session("t", "").unwrap();
        let ep_a = db.add_episode(&session.id, "summary-a").unwrap();
        let ep_b = db.add_episode(&session.id, "summary-b").unwrap();
        let ep_c = db.add_episode(&session.id, "summary-c").unwrap();

        let missing = db
            .missing_embedding_ids_limited(entity_kind::EPISODE, "m", 2)
            .unwrap();
        assert_eq!(missing.len(), 2);
        assert!(
            missing
                .iter()
                .all(|id| id == &ep_a || id == &ep_b || id == &ep_c),
            "only memory_items should be missing-index candidates, got {:?}",
            missing
        );

        let missing3 = db
            .missing_embedding_ids_limited(entity_kind::EPISODE, "m", 3)
            .unwrap();
        assert_eq!(missing3.len(), 3);
        assert!(missing3.contains(&ep_a) && missing3.contains(&ep_b) && missing3.contains(&ep_c));
    }

    #[test]
    fn episode_text_resolves_message() {
        let db = db();
        let session = db.create_session("t", "").unwrap();
        let ep = db.add_episode(&session.id, "remember this").unwrap();
        assert_eq!(db.episode_text(&ep).unwrap(), Some("remember this".into()));
        assert_eq!(db.episode_text("nope").unwrap(), None);
    }

    #[test]
    fn prune_removes_orphaned() {
        let db = db();
        let session = db.create_session("t", "").unwrap();
        let ep = db.add_episode(&session.id, "hello").unwrap();
        db.save_embedding(entity_kind::EPISODE, &ep, "m", &[1.0], "hello")
            .unwrap();
        db.save_embedding(entity_kind::EPISODE, "ghost", "m", &[1.0], "gone")
            .unwrap();
        db.save_embedding(entity_kind::FACT, "ghost-fact", "m", &[1.0], "gone")
            .unwrap();
        let deleted = db.prune_orphaned_embeddings().unwrap();
        assert_eq!(deleted, 2);
        assert!(
            db.get_embedding(entity_kind::EPISODE, &ep)
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn list_embedding_models_distinct() {
        let db = db();
        insert_fact_with_id(&db, "f1");
        insert_fact_with_id(&db, "f2");
        insert_episode_with_id(&db, "e1");
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
        insert_fact_with_id(&db, "f1");
        insert_episode_with_id(&db, "e1");
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
        db.add_episode(&session.id, "I discussed the dark theme preference earlier")
            .unwrap();
        db.add_episode(&session.id, "unrelated note about groceries")
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
        db.add_episode(&session.id, "hello world").unwrap();
        assert!(db.search_episodes_by_keywords(&[], 5).unwrap().is_empty());
    }

    #[test]
    fn search_episodes_by_keywords_excludes_current_session() {
        let db = db();
        let current = db.create_session("current", "").unwrap();
        let past = db.create_session("past", "").unwrap();
        db.add_episode(&current.id, "I discussed the dark theme in this session")
            .unwrap();
        db.add_episode(&past.id, "I discussed the dark theme last week")
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
        let ep = db
            .add_episode(&session.id, "user prefers the dark theme everywhere")
            .unwrap();

        let missing = db.missing_embedding_ids(entity_kind::EPISODE, "m").unwrap();
        assert_eq!(missing, vec![ep.clone()]);
        assert_eq!(
            db.episode_text(&ep).unwrap().as_deref(),
            Some("user prefers the dark theme everywhere")
        );

        let hits = db
            .search_episodes_by_keywords(&["dark", "theme"], 5)
            .unwrap();
        assert!(hits.iter().any(|h| h.contains("dark theme")));

        db.save_embedding(
            entity_kind::EPISODE,
            &ep,
            "m",
            &[1.0, 0.0],
            "user prefers the dark theme everywhere",
        )
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
        let hits = db.search_episodes_by_keywords(&["hardware"], 5).unwrap();
        assert!(
            hits.iter().any(|h| h.contains("monitors")),
            "topic tag must be FTS-visible; got {hits:?}"
        );
    }

    #[test]
    fn embedding_list_is_cached_and_invalidated_on_write() {
        let db = db();
        insert_fact_with_id(&db, "f1");
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
        // Single-valued correction demotes confidence only — SPO text is
        // unchanged, so the embedding must stay (invalidate only on SPO).
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
                .is_some(),
            "confidence demotion must keep SPO embedding"
        );
        // SPO change drops the vector.
        {
            let conn = db.conn();
            conn.execute(
                "UPDATE memory_edges SET object = 'French' WHERE id = ?1",
                rusqlite::params![fact.id],
            )
            .unwrap();
        }
        assert!(
            db.get_embedding(entity_kind::FACT, &fact.id)
                .unwrap()
                .is_none(),
            "SPO UPDATE must drop the embedding"
        );
        // Re-embed, then DELETE drops it again.
        db.save_embedding(
            entity_kind::FACT,
            &fact.id,
            "m",
            &[1.0],
            "user language French",
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

    #[test]
    fn lsh_bucket_is_deterministic() {
        let v = vec![0.1f32, -0.2, 0.5, 0.0, 1.0];
        assert_eq!(lsh_bucket(&v), lsh_bucket(&v));
        assert_eq!(lsh_bucket(&[]), 0);
    }

    #[test]
    fn save_embedding_writes_lsh_bucket() {
        let db = db();
        insert_fact_with_id(&db, "f1");
        db.save_embedding(entity_kind::FACT, "f1", "m", &[1.0, 0.0, 0.5], "a")
            .unwrap();
        let bucket: i64 = db
            .conn()
            .query_row(
                "SELECT bucket FROM embedding_lsh WHERE entity_type = ?1 AND entity_id = ?2 AND model = ?3",
                rusqlite::params![entity_kind::FACT, "f1", "m"],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(bucket, lsh_bucket(&[1.0, 0.0, 0.5]));
        assert_eq!(
            db.count_embeddings_for_model(entity_kind::FACT, "m")
                .unwrap(),
            1
        );
        assert_eq!(
            db.count_embeddings_for_model(entity_kind::FACT, "")
                .unwrap(),
            0
        );
    }

    #[test]
    fn lsh_probe_path_returns_near_neighbors() {
        let db = db();
        insert_fact_with_id(&db, "near");
        insert_fact_with_id(&db, "far");
        let target = vec![1.0f32, 0.0, 0.0, 0.0];
        db.save_embedding(entity_kind::FACT, "near", "m", &target, "near")
            .unwrap();
        db.save_embedding(entity_kind::FACT, "far", "m", &[-1.0, 0.0, 0.0, 0.0], "far")
            .unwrap();
        // Force LSH path regardless of ANN_ACTIVATE_MIN.
        let candidates = db
            .list_embeddings_via_lsh(entity_kind::FACT, "m", &target, 16, None, None)
            .unwrap();
        assert!(
            candidates.iter().any(|e| e.entity_id == "near"),
            "LSH probe must include the identical vector"
        );
        let hits = db
            .search_embeddings_filtered(entity_kind::FACT, &target, 1, "m", None, None)
            .unwrap();
        assert_eq!(hits[0].0.entity_id, "near");
    }

    #[test]
    fn rebuild_embedding_lsh_refills_side_table() {
        let db = db();
        insert_fact_with_id(&db, "f1");
        db.save_embedding(entity_kind::FACT, "f1", "m", &[0.25, 0.75], "x")
            .unwrap();
        db.conn().execute("DELETE FROM embedding_lsh", []).unwrap();
        let n = db.rebuild_embedding_lsh("m").unwrap();
        assert_eq!(n, 1);
        let bucket: i64 = db
            .conn()
            .query_row(
                "SELECT bucket FROM embedding_lsh WHERE entity_id = 'f1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(bucket, lsh_bucket(&[0.25, 0.75]));
    }
}
