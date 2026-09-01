//! Typed memory read and retrieval boundary.
//!
//! CRUD repositories remain the persistence authority. This module owns the
//! common read policy used by the agent prompt, the memory tool, and the
//! desktop recall command: query normalization, keyword fallback, optional
//! vector fusion, deterministic ordering, and defense-in-depth filtering.

use crate::Database;
use crate::embeddings::{EmbeddedText, entity_kind};
use crate::repositories::facts::{
    Fact, fact_effective_confidence, is_sensitive_object, is_sensitive_predicate,
};
use std::collections::{HashMap, HashSet};

/// Maximum result count accepted by the shared recall contract.
pub const MAX_RECALL_LIMIT: usize = 20;

/// Memory entity domain used by the shared recall contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MemoryKind {
    Fact,
    Episode,
}

impl MemoryKind {
    pub fn parse(value: &str) -> anyhow::Result<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "fact" | "facts" => Ok(Self::Fact),
            "episode" | "episodes" => Ok(Self::Episode),
            _ => anyhow::bail!("kind must be fact or episode"),
        }
    }

    pub const fn entity_type(self) -> &'static str {
        match self {
            Self::Fact => entity_kind::FACT,
            Self::Episode => entity_kind::EPISODE,
        }
    }
}

/// One typed memory retrieval request. The optional scopes are kept here so
/// every caller applies current-session exclusion and fact subject narrowing
/// through the same read path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryQuery {
    pub text: String,
    pub kind: MemoryKind,
    pub limit: usize,
    pub exclude_session_id: Option<String>,
    pub fact_subject: Option<String>,
}

impl MemoryQuery {
    pub fn new(text: &str, kind: MemoryKind, limit: usize) -> anyhow::Result<Self> {
        let text = text.trim();
        if text.is_empty() {
            anyhow::bail!("query is required for memory recall");
        }
        Ok(Self {
            text: text.to_string(),
            kind,
            limit: limit.clamp(1, MAX_RECALL_LIMIT),
            exclude_session_id: None,
            fact_subject: None,
        })
    }

    pub fn with_excluded_session(mut self, session_id: Option<&str>) -> Self {
        self.exclude_session_id = session_id
            .map(str::trim)
            .filter(|id| !id.is_empty())
            .map(str::to_string);
        self
    }

    pub fn with_fact_subject(mut self, subject: Option<&str>) -> Self {
        self.fact_subject = subject
            .map(str::trim)
            .filter(|subject| !subject.is_empty())
            .map(str::to_string);
        self
    }
}

/// A stable, JSON-friendly memory result. This is the internal contract too;
/// callers should not pass `serde_json::Value` across the memory boundary.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct MemoryHit {
    pub entity_id: String,
    pub text: String,
    pub score: f64,
    pub model: String,
}

impl MemoryHit {
    pub fn from_embedding(embedding: EmbeddedText, score: f64) -> Self {
        Self {
            entity_id: embedding.entity_id,
            text: embedding.text,
            score,
            model: embedding.model,
        }
    }
}

/// How the shared retriever produced a result set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryRecallMode {
    Keyword,
    Hybrid,
}

/// Typed result from the shared retriever.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct MemoryRecall {
    pub hits: Vec<MemoryHit>,
    pub mode: MemoryRecallMode,
}

impl Default for MemoryRecall {
    fn default() -> Self {
        Self {
            hits: Vec::new(),
            mode: MemoryRecallMode::Keyword,
        }
    }
}

/// Read/retrieve facade over the database. It deliberately has no embedding
/// provider dependency: callers acquire a vector through their configured
/// router, then hand the vector to this common persistence/ranking boundary.
pub struct MemoryRetriever<'db> {
    db: &'db Database,
}

impl<'db> MemoryRetriever<'db> {
    pub fn new(db: &'db Database) -> Self {
        Self { db }
    }

    /// Defense-in-depth visibility policy shared by CRUD search, prompt
    /// recall, and vector recall. Stored credential-like rows are never
    /// returned to a model even if a legacy database contains them.
    pub fn visible_fact(fact: &Fact) -> bool {
        !is_sensitive_predicate(&fact.predicate) && !is_sensitive_object(&fact.object)
    }

    pub fn filter_visible_facts(facts: impl IntoIterator<Item = Fact>) -> Vec<Fact> {
        facts.into_iter().filter(Self::visible_fact).collect()
    }

    pub fn visible_text(text: &str) -> bool {
        !crate::repositories::facts::is_sensitive_text(text)
    }

    /// Read keyword candidates only. This is intentionally separate from
    /// vector retrieval so prompt rendering can merge both candidate pools
    /// without repeating SQL or embedding work.
    pub fn keyword(&self, query: &MemoryQuery) -> anyhow::Result<Vec<MemoryHit>> {
        let hits = match query.kind {
            MemoryKind::Fact => {
                let terms = haven_common::text::memory_recall_terms(&query.text);
                let term_refs = haven_common::text::memory_recall_term_sample(&terms, 6);
                let facts = self
                    .db
                    .search_facts_any(&term_refs, query.limit)?
                    .into_iter()
                    .filter(|fact| {
                        query
                            .fact_subject
                            .as_deref()
                            .is_none_or(|subject| fact.subject == subject)
                    })
                    .collect::<Vec<_>>();
                Self::filter_visible_facts(facts)
                    .into_iter()
                    .map(|fact| {
                        let score = fact_effective_confidence(&fact);
                        MemoryHit {
                            entity_id: fact.id,
                            text: format!("{}={}", fact.predicate, fact.object),
                            score,
                            model: String::new(),
                        }
                    })
                    .collect()
            }
            MemoryKind::Episode => self
                .db
                .search_episodes_by_keywords_excluding(
                    &haven_common::text::memory_recall_term_sample(
                        &haven_common::text::memory_recall_terms(&query.text),
                        6,
                    ),
                    query.limit,
                    query.exclude_session_id.as_deref(),
                )?
                .into_iter()
                .filter(|text| Self::visible_text(text))
                .map(|text| MemoryHit {
                    entity_id: String::new(),
                    text,
                    // Preserve the existing wire meaning for keyword episode
                    // hits: only vector recall exposes a similarity score.
                    score: 0.0,
                    model: String::new(),
                })
                .collect(),
        };
        Ok(hits)
    }

    /// Read vector candidates from the shared embedding persistence path.
    /// The model is mandatory so mixed embedding spaces cannot leak into a
    /// result set. Fact rows are rehydrated before filtering to protect
    /// against sensitive legacy rows whose embedding text is incomplete.
    pub fn vector(
        &self,
        query: &MemoryQuery,
        vector: &[f32],
        model: &str,
    ) -> anyhow::Result<Vec<MemoryHit>> {
        if vector.is_empty() || model.trim().is_empty() {
            return Ok(Vec::new());
        }
        let raw = self.db.search_embeddings_filtered(
            query.kind.entity_type(),
            vector,
            query.limit,
            model,
            query.fact_subject.as_deref(),
            query.exclude_session_id.as_deref(),
        )?;
        let mut hits: Vec<MemoryHit> = match query.kind {
            MemoryKind::Fact => {
                let ids: Vec<String> = raw
                    .iter()
                    .map(|(embedding, _)| embedding.entity_id.clone())
                    .collect();
                let facts_by_id: HashMap<String, Fact> = self
                    .db
                    .get_facts_by_ids(&ids)?
                    .into_iter()
                    .map(|fact| (fact.id.clone(), fact))
                    .collect();
                raw.into_iter()
                    .filter_map(|(embedding, score)| {
                        let fact = facts_by_id.get(&embedding.entity_id)?;
                        Self::visible_fact(fact).then(|| MemoryHit {
                            entity_id: fact.id.clone(),
                            text: format!("{}={}", fact.predicate, fact.object),
                            score,
                            model: embedding.model,
                        })
                    })
                    .collect()
            }
            MemoryKind::Episode => raw
                .into_iter()
                .filter(|(embedding, _)| Self::visible_text(&embedding.text))
                .map(|(embedding, score)| MemoryHit::from_embedding(embedding, score))
                .collect(),
        };
        hits.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.entity_id.cmp(&b.entity_id))
                .then_with(|| a.text.cmp(&b.text))
        });
        Ok(hits)
    }

    /// Combine keyword and vector candidates using one deterministic identity
    /// rule. Empty episode ids from the legacy keyword repository are keyed by
    /// text, while facts and vector episodes use their stable entity id.
    pub fn merge(
        &self,
        query: &MemoryQuery,
        keyword_hits: Vec<MemoryHit>,
        vector_hits: Vec<MemoryHit>,
    ) -> MemoryRecall {
        let has_vectors = !vector_hits.is_empty();
        let mut seen = HashSet::new();
        let mut hits = Vec::with_capacity(query.limit);
        // Semantic hits lead; keyword hits fill the remaining slots. This
        // keeps vector recall useful while retaining the no-embedding path.
        for hit in vector_hits.into_iter().chain(keyword_hits) {
            let key = if hit.entity_id.is_empty() {
                format!("text:{}", hit.text)
            } else {
                format!("id:{}", hit.entity_id)
            };
            if seen.insert(key) {
                hits.push(hit);
                if hits.len() >= query.limit {
                    break;
                }
            }
        }
        MemoryRecall {
            hits,
            mode: if has_vectors {
                MemoryRecallMode::Hybrid
            } else {
                MemoryRecallMode::Keyword
            },
        }
    }

    /// Full shared retrieval. Vector acquisition is optional and failures are
    /// represented by an empty vector candidate pool, so keyword recall is a
    /// reliable degradation path.
    pub fn retrieve(
        &self,
        query: &MemoryQuery,
        vector_hits: Option<Vec<MemoryHit>>,
    ) -> anyhow::Result<MemoryRecall> {
        let keyword_hits = self.keyword(query)?;
        let had_vectors = vector_hits.as_ref().is_some_and(|hits| !hits.is_empty());
        let vector_hits = vector_hits.unwrap_or_default();
        let mut recall = self.merge(query, keyword_hits, vector_hits);
        if had_vectors {
            recall.mode = MemoryRecallMode::Hybrid;
        }
        Ok(recall)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keyword_recall_is_typed_and_filters_sensitive_facts() {
        let db = Database::open_in_memory().unwrap();
        let safe = db
            .insert_fact("user", "likes", "Rust", "user", 0.9, &["preference"])
            .unwrap();
        db.insert_fact("user", "api_key", "super-secret", "user", 1.0, &[])
            .unwrap();

        let query = MemoryQuery::new("Rust", MemoryKind::Fact, 5).unwrap();
        let recall = MemoryRetriever::new(&db).retrieve(&query, None).unwrap();

        assert_eq!(recall.mode, MemoryRecallMode::Keyword);
        assert_eq!(recall.hits.len(), 1);
        assert_eq!(recall.hits[0].entity_id, safe.id);
        assert_eq!(recall.hits[0].text, "likes=Rust");
    }

    #[test]
    fn vector_recall_is_typed_hybrid_and_filters_legacy_sensitive_rows() {
        let db = Database::open_in_memory().unwrap();
        let safe = db
            .insert_fact("user", "likes", "Rust", "inferred", 0.8, &[])
            .unwrap();
        let secret = db
            .insert_fact("user", "api_key", "super-secret", "inferred", 0.8, &[])
            .unwrap();
        db.save_embedding(
            entity_kind::FACT,
            &safe.id,
            "model-a",
            &[1.0, 0.0],
            "likes Rust",
        )
        .unwrap();
        db.save_embedding(
            entity_kind::FACT,
            &secret.id,
            "model-a",
            &[1.0, 0.0],
            "api_key super-secret",
        )
        .unwrap();

        let query = MemoryQuery::new("programming", MemoryKind::Fact, 5).unwrap();
        let vector_hits = MemoryRetriever::new(&db)
            .vector(&query, &[1.0, 0.0], "model-a")
            .unwrap();
        let recall = MemoryRetriever::new(&db)
            .retrieve(&query, Some(vector_hits))
            .unwrap();

        assert_eq!(recall.mode, MemoryRecallMode::Hybrid);
        assert_eq!(recall.hits.len(), 1);
        assert_eq!(recall.hits[0].entity_id, safe.id);
        assert!(!recall.hits.iter().any(|hit| hit.entity_id == secret.id));
    }

    #[test]
    fn empty_vector_degrades_to_keyword_recall() {
        let db = Database::open_in_memory().unwrap();
        let fact = db
            .insert_fact("user", "uses", "SQLite", "user", 1.0, &[])
            .unwrap();
        let query = MemoryQuery::new("SQLite", MemoryKind::Fact, 5).unwrap();
        let recall = MemoryRetriever::new(&db)
            .retrieve(&query, Some(Vec::new()))
            .unwrap();

        assert_eq!(recall.mode, MemoryRecallMode::Keyword);
        assert_eq!(recall.hits[0].entity_id, fact.id);
        assert!(recall.hits[0].model.is_empty());
    }
}
