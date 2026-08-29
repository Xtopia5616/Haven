//! Graph mutation boundary for memory facts.
//!
//! `facts.rs` owns fact reads, search and ranking. This module owns the
//! mutation semantics for `memory_edges`, including node linking, provenance
//! storage, user authority and inferred-fact reinforcement. Keeping those
//! rules together prevents a new write path from bypassing graph invariants.

use super::facts::{
    FACT_COLS, Fact, FactSourceRef, UpsertOutcome, fact_from_row, is_single_valued_predicate,
    normalize_predicate, polarity_opposite,
};
use crate::db::Database;
use chrono::Utc;

const CONTRADICTION_DEMOTE_FACTOR: f64 = super::facts::CONTRADICTION_DEMOTE_FACTOR;

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
            source_ref: source_ref.cloned(),
            durability,
        })
    }

    /// Map a public source reference to either the memory-item FK or an
    /// opaque transcript reference. The snippet is always retained.
    fn provenance_cols_from_source_ref(
        &self,
        source_ref: Option<&FactSourceRef>,
    ) -> anyhow::Result<(Option<String>, Option<String>, Option<String>)> {
        let Some(refer) = source_ref else {
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
            .unwrap_or(false);
        if in_items {
            Ok((Some(refer.message_id.clone()), None, snippet))
        } else {
            Ok((None, Some(refer.message_id.clone()), snippet))
        }
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
        let triple_exists: Option<Fact> = {
            let conn = self.db.conn();
            conn.query_row(
                &format!(
                    "SELECT {FACT_COLS} FROM memory_edges WHERE subject = ?1 AND predicate = ?2 AND object = ?3"
                ),
                rusqlite::params![subject, predicate, object],
                fact_from_row,
            )
            .ok()
        };
        if let Some(existing) = triple_exists {
            if existing.source == "user" {
                let now = Utc::now().to_rfc3339();
                {
                    let conn = self.db.conn();
                    conn.execute(
                        "UPDATE memory_edges
                         SET mention_count = mention_count + 1, last_seen_at = ?1, confidence = 1.0,
                             durability = 1.0
                         WHERE id = ?2",
                        rusqlite::params![now, existing.id],
                    )?;
                }
                self.db.cache_invalidate_facts(subject);
                self.db
                    .cache_invalidate_embeddings(crate::embeddings::entity_kind::FACT);
                let mut fact = existing;
                fact.confidence = 1.0;
                fact.durability = 1.0;
                fact.mention_count += 1;
                fact.last_seen_at = Some(now);
                return Ok(fact);
            }
            let now = Utc::now().to_rfc3339();
            {
                let conn = self.db.conn();
                conn.execute(
                    "UPDATE memory_edges SET source = 'user', confidence = 1.0, last_seen_at = ?1, durability = 1.0 WHERE id = ?2",
                    rusqlite::params![now, existing.id],
                )?;
            }
            self.db.cache_invalidate_facts(subject);
            self.db
                .cache_invalidate_embeddings(crate::embeddings::entity_kind::FACT);
            let mut fact = existing;
            fact.source = "user".into();
            fact.confidence = 1.0;
            fact.durability = 1.0;
            fact.last_seen_at = Some(now);
            return Ok(fact);
        }
        if is_single_valued_predicate(&predicate) {
            let conn = self.db.conn();
            conn.execute(
                "DELETE FROM memory_edges WHERE subject = ?1 AND predicate = ?2",
                rusqlite::params![subject, predicate],
            )?;
            self.db
                .cache_invalidate_embeddings(crate::embeddings::entity_kind::FACT);
        }
        self.insert(subject, &predicate, object, "user", 1.0, tags)
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
            .ok()
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
                .ok();
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
                    .map(|_| true)
                    .unwrap_or(false);
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
            .ok();
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
