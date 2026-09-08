//! Graph mutation boundary for memory facts.
//!
//! `facts.rs` owns fact reads, search and ranking. This module owns the
//! mutation semantics for `memory_edges`, including node linking, provenance
//! storage, user authority and inferred-fact reinforcement. Keeping those
//! rules together prevents a new write path from bypassing graph invariants.

use super::fact_query::{FACT_COLS, fact_from_row};
use super::facts::{
    Fact, FactSourceRef, UpsertOutcome, is_sensitive_text, is_single_valued_predicate,
    normalize_predicate, polarity_opposite,
};
use crate::db::Database;
use chrono::Utc;
use rusqlite::OptionalExtension;

const CONTRADICTION_DEMOTE_FACTOR: f64 = super::facts::CONTRADICTION_DEMOTE_FACTOR;
const PROVENANCE_SNIPPET_MAX_CHARS: usize = 120;

fn serialize_tags(tags: &[&str]) -> String {
    serde_json::to_string(tags).unwrap_or_else(|_| "[]".into())
}

fn node_kind_for_label(label: &str) -> &'static str {
    if label.eq_ignore_ascii_case("user") {
        "user"
    } else {
        "concept"
    }
}

fn sanitize_source_ref(source_ref: Option<&FactSourceRef>) -> Option<FactSourceRef> {
    let refer = source_ref?;
    let mut safe = refer.clone();
    safe.snippet = if safe.snippet.trim().is_empty() {
        String::new()
    } else if is_sensitive_text(&safe.snippet) {
        "[redacted]".to_string()
    } else {
        safe.snippet
            .chars()
            .take(PROVENANCE_SNIPPET_MAX_CHARS)
            .collect()
    };
    Some(safe)
}

fn validate_fact_fields(
    subject: &str,
    predicate: &str,
    object: &str,
    source: &str,
    confidence: f64,
    durability: f64,
) -> anyhow::Result<()> {
    anyhow::ensure!(!subject.trim().is_empty(), "fact subject is required");
    anyhow::ensure!(!predicate.trim().is_empty(), "fact predicate is required");
    anyhow::ensure!(!object.trim().is_empty(), "fact object is required");
    anyhow::ensure!(
        matches!(source, "user" | "inferred"),
        "unsupported fact source '{source}'"
    );
    anyhow::ensure!(
        confidence.is_finite() && (0.0..=1.0).contains(&confidence),
        "fact confidence must be finite and between 0 and 1"
    );
    anyhow::ensure!(
        durability.is_finite() && (0.0..=1.0).contains(&durability),
        "fact durability must be finite and between 0 and 1"
    );
    Ok(())
}

/// Internal writer for the typed memory graph's fact edges.
///
/// This is deliberately a small, non-`pub`-API facade. Callers continue to
/// use the stable `Database` methods while all `memory_edges` mutations are
/// implemented in one place.
pub(crate) struct FactGraph<'db> {
    db: &'db Database,
}

impl<'db> FactGraph<'db> {
    pub(crate) fn new(db: &'db Database) -> Self {
        Self { db }
    }

    pub(crate) fn insert(
        &self,
        subject: &str,
        predicate: &str,
        object: &str,
        source: &str,
        confidence: f64,
        tags: &[&str],
    ) -> anyhow::Result<Fact> {
        self.insert_with_source_ref(
            subject, predicate, object, source, confidence, tags, None, 1.0,
        )
    }

    /// Insert a fact with an optional reference to the message it came from
    /// and an explicit durability rating (0..1).
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn insert_with_source_ref(
        &self,
        subject: &str,
        predicate: &str,
        object: &str,
        source: &str,
        confidence: f64,
        tags: &[&str],
        source_ref: Option<&FactSourceRef>,
        durability: f64,
    ) -> anyhow::Result<Fact> {
        let predicate = normalize_predicate(predicate);
        validate_fact_fields(subject, &predicate, object, source, confidence, durability)?;
        let safe_source_ref = sanitize_source_ref(source_ref);
        let id = haven_common::types::new_id("fact");
        let now = Utc::now().to_rfc3339();
        let tags_json = serialize_tags(tags);
        let subject_id = self.db.ensure_node(node_kind_for_label(subject), subject)?;
        let object_id = self.db.ensure_node(node_kind_for_label(object), object)?;
        let (prov_item, prov_record, prov_snippet) =
            self.provenance_cols_from_source_ref(source_ref)?;
        let conn = self.db.conn();
        conn.execute(
            "INSERT INTO memory_edges (
                id, subject, subject_id, predicate, object, object_id,
                source, confidence, created_at, tags, mention_count, last_seen_at,
                provenance_item_id, provenance_record_id, provenance_snippet, durability
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, 0, ?11, ?12, ?13, ?14, ?15)",
            rusqlite::params![
                id,
                subject,
                subject_id,
                predicate,
                object,
                object_id,
                source,
                confidence,
                now,
                tags_json,
                now,
                prov_item,
                prov_record,
                prov_snippet,
                durability
            ],
        )?;
        self.db.cache_invalidate_facts(subject);
        Ok(Fact {
            id,
            subject: subject.into(),
            predicate,
            object: object.into(),
            source: source.into(),
            confidence,
            tags: tags.iter().map(|s| s.to_string()).collect(),
            created_at: now.clone(),
            mention_count: 0,
            last_seen_at: Some(now),
            source_ref: safe_source_ref,
            durability,
        })
    }

    /// Map a public source reference to either the memory-item FK or an
    /// opaque transcript reference. Snippets are bounded and redacted before
    /// they reach durable storage.
    fn provenance_cols_from_source_ref(
        &self,
        source_ref: Option<&FactSourceRef>,
    ) -> anyhow::Result<(Option<String>, Option<String>, Option<String>)> {
        let sanitized = sanitize_source_ref(source_ref);
        let Some(refer) = sanitized.as_ref() else {
            return Ok((None, None, None));
        };
        let snippet = if refer.snippet.is_empty() {
            None
        } else {
            Some(refer.snippet.clone())
        };
        if refer.message_id.is_empty() {
            return Ok((None, None, snippet));
        }
        let conn = self.db.conn();
        let in_items = conn
            .query_row(
                "SELECT 1 FROM memory_items WHERE id = ?1",
                rusqlite::params![refer.message_id],
                |_| Ok(true),
            )
            .optional()?
            .is_some();
        if in_items {
            Ok((Some(refer.message_id.clone()), None, snippet))
        } else {
            Ok((None, Some(refer.message_id.clone()), snippet))
        }
    }

    fn insert_user_row(
        conn: &rusqlite::Connection,
        subject: &str,
        subject_id: &str,
        predicate: &str,
        object: &str,
        object_id: &str,
        tags: &[&str],
    ) -> anyhow::Result<Fact> {
        let id = haven_common::types::new_id("fact");
        let now = Utc::now().to_rfc3339();
        let tags_json = serialize_tags(tags);
        conn.execute(
            "INSERT INTO memory_edges (
                id, subject, subject_id, predicate, object, object_id,
                source, confidence, created_at, tags, mention_count, last_seen_at,
                durability
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'user', 1.0, ?7, ?8, 0, ?7, 1.0)",
            rusqlite::params![
                id, subject, subject_id, predicate, object, object_id, now, tags_json
            ],
        )?;
        Ok(Fact {
            id,
            subject: subject.into(),
            predicate: predicate.into(),
            object: object.into(),
            source: "user".into(),
            confidence: 1.0,
            tags: tags.iter().map(|tag| (*tag).to_string()).collect(),
            created_at: now.clone(),
            mention_count: 0,
            last_seen_at: Some(now),
            source_ref: None,
            durability: 1.0,
        })
    }

    /// Store a fact explicitly stated by the user. User facts are
    /// authoritative and single-valued predicates replace all prior values.
    pub(crate) fn set_user(
        &self,
        subject: &str,
        predicate: &str,
        object: &str,
        tags: &[&str],
    ) -> anyhow::Result<Fact> {
        let predicate = normalize_predicate(predicate);
        validate_fact_fields(subject, &predicate, object, "user", 1.0, 1.0)?;
        // Resolve node ids before opening the edge transaction. Node creation is
        // independently idempotent; the transaction below makes the user-edge
        // replacement itself atomic, so a failed insert cannot leave a missing
        // single-valued fact after the old value was deleted.
        let subject_id = self.db.ensure_node(node_kind_for_label(subject), subject)?;
        let object_id = self.db.ensure_node(node_kind_for_label(object), object)?;
        let conn = self.db.conn();
        conn.execute_batch("BEGIN IMMEDIATE")?;
        let result = (|| -> anyhow::Result<(Fact, bool)> {
            let existing: Option<Fact> = conn
                .query_row(
                    &format!(
                        "SELECT {FACT_COLS} FROM memory_edges WHERE subject = ?1 AND predicate = ?2 AND object = ?3"
                    ),
                    rusqlite::params![subject, predicate, object],
                    fact_from_row,
                )
                .optional()?;
            if let Some(existing) = existing {
                let now = Utc::now().to_rfc3339();
                if existing.source == "user" {
                    conn.execute(
                        "UPDATE memory_edges
                         SET mention_count = mention_count + 1, last_seen_at = ?1, confidence = 1.0,
                             durability = 1.0
                         WHERE id = ?2",
                        rusqlite::params![now, existing.id],
                    )?;
                    let mut fact = existing;
                    fact.confidence = 1.0;
                    fact.durability = 1.0;
                    fact.mention_count += 1;
                    fact.last_seen_at = Some(now);
                    return Ok((fact, true));
                }
                conn.execute(
                    "UPDATE memory_edges
                     SET source = 'user', confidence = 1.0, last_seen_at = ?1, durability = 1.0
                     WHERE id = ?2",
                    rusqlite::params![now, existing.id],
                )?;
                let mut fact = existing;
                fact.source = "user".into();
                fact.confidence = 1.0;
                fact.durability = 1.0;
                fact.last_seen_at = Some(now);
                return Ok((fact, true));
            }
            let replaced = if is_single_valued_predicate(&predicate) {
                conn.execute(
                    "DELETE FROM memory_edges WHERE subject = ?1 AND predicate = ?2",
                    rusqlite::params![subject, predicate],
                )? > 0
            } else {
                false
            };
            let fact = Self::insert_user_row(
                &conn,
                subject,
                &subject_id,
                &predicate,
                object,
                &object_id,
                tags,
            )?;
            Ok((fact, replaced))
        })();
        let result = match result {
            Ok(result) => {
                conn.execute_batch("COMMIT")?;
                result
            }
            Err(error) => {
                let _ = conn.execute_batch("ROLLBACK");
                return Err(error);
            }
        };
        self.db.cache_invalidate_facts(subject);
        if result.1 {
            self.db
                .cache_invalidate_embeddings(crate::embeddings::entity_kind::FACT);
        }
        Ok(result.0)
    }

    pub(crate) fn delete_by_triple(
        &self,
        subject: &str,
        predicate: &str,
        object: Option<&str>,
    ) -> anyhow::Result<u64> {
        let predicate = normalize_predicate(predicate);
        let conn = self.db.conn();
        let deleted = match object {
            Some(obj) => conn.execute(
                "DELETE FROM memory_edges WHERE subject = ?1 AND predicate = ?2 AND object = ?3",
                rusqlite::params![subject, predicate, obj],
            )?,
            None => conn.execute(
                "DELETE FROM memory_edges WHERE subject = ?1 AND predicate = ?2",
                rusqlite::params![subject, predicate],
            )?,
        };
        self.db.cache_invalidate_facts(subject);
        self.db
            .cache_invalidate_embeddings(crate::embeddings::entity_kind::FACT);
        Ok(deleted as u64)
    }

    pub(crate) fn ensure(
        &self,
        subject: &str,
        predicate: &str,
        object: &str,
        source: &str,
        confidence: f64,
        tags: &[&str],
    ) -> anyhow::Result<Fact> {
        let existing: Option<Fact> = {
            let conn = self.db.conn();
            conn.query_row(
                &format!(
                    "SELECT {FACT_COLS} FROM memory_edges WHERE subject = ?1 AND predicate = ?2 AND object = ?3"
                ),
                rusqlite::params![subject, predicate, object],
                fact_from_row,
            )
            .optional()?
        };
        if let Some(existing) = existing {
            return Ok(existing);
        }
        self.insert(subject, predicate, object, source, confidence, tags)
    }

    /// Insert, reinforce or correct an inferred fact while enforcing user
    /// authority, single-valued predicates and polarity demotion.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn upsert(
        &self,
        subject: &str,
        predicate: &str,
        object: &str,
        source: &str,
        confidence: f64,
        tags: &[&str],
        source_ref: Option<&FactSourceRef>,
        durability: f64,
    ) -> anyhow::Result<UpsertOutcome> {
        let predicate = normalize_predicate(predicate);
        validate_fact_fields(subject, &predicate, object, source, confidence, durability)?;
        let now = Utc::now().to_rfc3339();
        let mut corrected = false;
        let opposite = polarity_opposite(&predicate);
        {
            let conn = self.db.conn();
            let existing: Option<Fact> = conn
                .query_row(
                    &format!(
                        "SELECT {FACT_COLS} FROM memory_edges WHERE subject = ?1 AND predicate = ?2 AND object = ?3"
                    ),
                    rusqlite::params![subject, predicate, object],
                    fact_from_row,
                )
                .optional()?;
            if let Some(existing) = existing {
                let boosted = (existing.confidence * 1.05).min(1.0).max(confidence);
                let merged_durability = existing.durability.max(durability).clamp(0.0, 1.0);
                let merged_ref = source_ref.or(existing.source_ref.as_ref());
                let mut merged_tags = existing.tags.clone();
                for tag in tags {
                    if !merged_tags.iter().any(|item| item == tag) {
                        merged_tags.push((*tag).to_string());
                    }
                }
                let tag_refs: Vec<&str> = merged_tags.iter().map(|s| s.as_str()).collect();
                let tags_json = serialize_tags(&tag_refs);
                let existing_id = existing.id.clone();
                drop(conn);
                let (prov_item, prov_record, prov_snippet) =
                    self.provenance_cols_from_source_ref(merged_ref)?;
                {
                    let conn = self.db.conn();
                    conn.execute(
                        "UPDATE memory_edges
                         SET mention_count = mention_count + 1, last_seen_at = ?1, confidence = ?2,
                             provenance_item_id = ?3, provenance_record_id = ?4,
                             provenance_snippet = ?5, tags = ?6, durability = ?7
                         WHERE id = ?8",
                        rusqlite::params![
                            now,
                            boosted,
                            prov_item,
                            prov_record,
                            prov_snippet,
                            tags_json,
                            merged_durability,
                            existing_id
                        ],
                    )?;
                }
                self.db.cache_invalidate_facts(subject);
                self.db
                    .cache_invalidate_embeddings(crate::embeddings::entity_kind::FACT);
                return Ok(UpsertOutcome::Reinforced);
            }

            if is_single_valued_predicate(&predicate) {
                let has_user_value = conn
                    .query_row(
                        "SELECT 1 FROM memory_edges WHERE subject = ?1 AND predicate = ?2 AND source = 'user' AND object <> ?3 LIMIT 1",
                        rusqlite::params![subject, predicate, object],
                        |r| r.get::<_, i32>(0),
                    )
                    .optional()?
                    .is_some();
                if has_user_value && source == "inferred" {
                    return Ok(UpsertOutcome::Skipped);
                }
                let n = conn.execute(
                    "UPDATE memory_edges SET confidence = confidence * ?1
                     WHERE subject = ?2 AND predicate = ?3 AND object <> ?4 AND source = 'inferred'",
                    rusqlite::params![CONTRADICTION_DEMOTE_FACTOR, subject, predicate, object],
                )?;
                corrected = n > 0;
            }

            if let Some(opp) = opposite {
                let incoming_is_user = (source == "user") as i32;
                let _ = conn.execute(
                    "UPDATE memory_edges SET confidence = confidence * ?1
                     WHERE subject = ?2 AND object = ?3 AND predicate = ?4
                       AND (?5 = 1 OR source = 'inferred')",
                    rusqlite::params![
                        CONTRADICTION_DEMOTE_FACTOR,
                        subject,
                        object,
                        opp,
                        incoming_is_user
                    ],
                )?;
            }
        }
        if corrected || opposite.is_some() {
            self.db
                .cache_invalidate_embeddings(crate::embeddings::entity_kind::FACT);
        }
        let _ = self.insert_with_source_ref(
            subject, &predicate, object, source, confidence, tags, source_ref, durability,
        )?;
        Ok(if corrected {
            UpsertOutcome::Corrected
        } else {
            UpsertOutcome::Inserted
        })
    }

    pub(crate) fn delete_by_id(&self, id: &str) -> anyhow::Result<()> {
        let conn = self.db.conn();
        let subject: Option<String> = conn
            .query_row(
                "SELECT subject FROM memory_edges WHERE id = ?1",
                rusqlite::params![id],
                |r| r.get(0),
            )
            .optional()?;
        conn.execute(
            "DELETE FROM memory_edges WHERE id = ?1",
            rusqlite::params![id],
        )?;
        if let Some(subject) = subject {
            self.db.cache_invalidate_facts(&subject);
        }
        self.db
            .cache_invalidate_embeddings(crate::embeddings::entity_kind::FACT);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::FactGraph;
    use crate::Database;
    use crate::repositories::facts::UpsertOutcome;

    #[test]
    fn insert_links_both_labels_to_graph_nodes() {
        let db = Database::open_in_memory().unwrap();
        let fact = FactGraph::new(&db)
            .insert("user", "likes", "Rust", "inferred", 0.8, &[])
            .unwrap();

        assert!(fact.id.starts_with("fact-"));
        let node_count: i64 = db
            .conn()
            .query_row(
                "SELECT COUNT(*) FROM memory_nodes WHERE label IN ('user', 'Rust')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(node_count, 2);
    }

    #[test]
    fn upsert_does_not_override_a_user_single_value() {
        let db = Database::open_in_memory().unwrap();
        let graph = FactGraph::new(&db);
        graph
            .set_user("user", "project_path", "D:/authoritative", &[])
            .unwrap();

        let outcome = graph
            .upsert(
                "user",
                "project_path",
                "D:/inferred",
                "inferred",
                0.9,
                &[],
                None,
                1.0,
            )
            .unwrap();

        assert_eq!(outcome, UpsertOutcome::Skipped);
        let count: i64 = db
            .conn()
            .query_row(
                "SELECT COUNT(*) FROM memory_edges WHERE predicate = 'project_path'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);
    }
}
