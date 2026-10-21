//! Typed memory read and retrieval boundary.
//!
//! CRUD repositories remain the persistence authority. This module owns the
//! common read policy used by the agent prompt, the memory tool, and the
//! desktop recall command: query normalization, keyword degradation, optional
//! vector fusion, deterministic ordering, and defense-in-depth filtering.

use crate::Database;
use crate::embeddings::{EmbeddedText, MemoryEmbeddingFilter};
use crate::repositories::fact_graph_recall::MAX_GRAPH_SEED_FACTS;
use crate::repositories::facts::{
    Fact, MemoryFactKind, fact_effective_confidence, filter_visible_facts, is_sensitive_text,
    is_visible_fact,
};
use std::collections::HashMap;

/// Maximum result count accepted by the shared recall contract.
pub const MAX_RECALL_LIMIT: usize = 20;

/// Maximum query size accepted by memory retrieval. Keeping this bound at the
/// shared boundary prevents an agent/tool/UI caller from turning a recall into
/// an unbounded FTS or embedding request.
pub const MAX_MEMORY_QUERY_CHARS: usize = 2_000;

/// Normalize and validate text used to query memory.
pub fn normalize_memory_query(text: &str) -> anyhow::Result<String> {
    let text = text.trim();
    if text.is_empty() {
        anyhow::bail!("query is required for memory recall");
    }
    if text.chars().count() > MAX_MEMORY_QUERY_CHARS {
        anyhow::bail!("memory query is too long (max {MAX_MEMORY_QUERY_CHARS} characters)");
    }
    Ok(text.split_whitespace().collect::<Vec<_>>().join(" "))
}

pub use crate::embeddings::MemoryEntityKind;

/// User-facing recall categories. These are semantic filters over the
/// canonical fact/episode storage domains, not additional embedding tables.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryRecallKind {
    Fact,
    Episode,
    Profile,
    Semantic,
    Procedural,
}

impl MemoryRecallKind {
    pub fn parse(value: &str) -> anyhow::Result<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "fact" => Ok(Self::Fact),
            "episode" => Ok(Self::Episode),
            "profile" => Ok(Self::Profile),
            "semantic" => Ok(Self::Semantic),
            "procedural" => Ok(Self::Procedural),
            _ => anyhow::bail!("kind must be fact, episode, profile, semantic, or procedural"),
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Fact => "fact",
            Self::Episode => "episode",
            Self::Profile => "profile",
            Self::Semantic => "semantic",
            Self::Procedural => "procedural",
        }
    }

    pub const fn storage_kind(self) -> MemoryEntityKind {
        match self {
            Self::Episode => MemoryEntityKind::Episode,
            Self::Fact | Self::Profile | Self::Semantic | Self::Procedural => {
                MemoryEntityKind::Fact
            }
        }
    }

    pub const fn fact_kind(self) -> Option<MemoryFactKind> {
        match self {
            Self::Profile => Some(MemoryFactKind::Profile),
            Self::Semantic => Some(MemoryFactKind::Semantic),
            Self::Procedural => Some(MemoryFactKind::Procedural),
            Self::Fact | Self::Episode => None,
        }
    }
}

impl From<MemoryEntityKind> for MemoryRecallKind {
    fn from(kind: MemoryEntityKind) -> Self {
        match kind {
            MemoryEntityKind::Fact => Self::Fact,
            MemoryEntityKind::Episode => Self::Episode,
        }
    }
}

/// One typed memory retrieval request. The optional scopes are kept here so
/// every caller applies current-session exclusion and fact subject narrowing
/// through the same read path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryQuery {
    text: String,
    kind: MemoryRecallKind,
    limit: usize,
    exclude_session_id: Option<String>,
    fact_subject: Option<String>,
}

impl MemoryQuery {
    pub fn new<K>(text: &str, kind: K, limit: usize) -> anyhow::Result<Self>
    where
        K: Into<MemoryRecallKind>,
    {
        let text = normalize_memory_query(text)?;
        anyhow::ensure!(limit > 0, "memory recall limit must be greater than zero");
        anyhow::ensure!(
            limit <= MAX_RECALL_LIMIT,
            "memory recall limit exceeds the maximum of {MAX_RECALL_LIMIT}"
        );
        Ok(Self {
            text,
            kind: kind.into(),
            limit,
            exclude_session_id: None,
            fact_subject: None,
        })
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn kind(&self) -> MemoryRecallKind {
        self.kind
    }

    pub fn limit(&self) -> usize {
        self.limit
    }

    pub fn excluded_session_id(&self) -> Option<&str> {
        self.exclude_session_id.as_deref()
    }

    pub fn fact_subject(&self) -> Option<&str> {
        self.fact_subject.as_deref()
    }

    pub fn with_excluded_session(mut self, session_id: Option<&str>) -> anyhow::Result<Self> {
        self.exclude_session_id = session_id
            .map(str::trim)
            .filter(|id| !id.is_empty())
            .map(str::to_string);
        if let Some(session_id) = self.exclude_session_id.as_deref() {
            anyhow::ensure!(
                haven_common::types::is_canonical_id(session_id, "ses"),
                "excluded session_id must be a canonical ses-* ID"
            );
        }
        Ok(self)
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
    pub(crate) fn from_embedding(embedding: EmbeddedText, score: f64) -> Self {
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

/// Why a recall returned no visible hits. This makes an empty result
/// actionable for the model instead of making `hits: []` look like proof that
/// the user's memory store is empty.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryRecallEmptyReason {
    NoIndex,
    NoHits,
    Filtered,
    EmbeddingFailed,
}

/// Whether a candidate source contributed to an empty recall. This is kept
/// typed so UI/tool consumers can distinguish “not configured” from “searched
/// and found nothing” without parsing prose.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryRecallSourceStatus {
    Hit,
    NoHits,
    NotConfigured,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryRecallSuggestion {
    BroadenQuery,
    RemoveSubjectFilter,
    TryOtherKind,
    ConfigureEmbeddings,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct MemoryRecallDiagnostics {
    pub keyword: MemoryRecallSourceStatus,
    pub vector: MemoryRecallSourceStatus,
    pub suggestions: Vec<MemoryRecallSuggestion>,
}

/// Typed result from the shared retriever.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct MemoryRecall {
    pub hits: Vec<MemoryHit>,
    pub mode: MemoryRecallMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub empty_reason: Option<MemoryRecallEmptyReason>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diagnostics: Option<MemoryRecallDiagnostics>,
}

impl Default for MemoryRecall {
    fn default() -> Self {
        Self {
            hits: Vec::new(),
            mode: MemoryRecallMode::Keyword,
            empty_reason: None,
            diagnostics: None,
        }
    }
}

/// Read/retrieve facade over the database. It deliberately has no embedding
/// provider dependency: callers acquire a vector through their configured
/// router, then hand the vector to this common persistence/ranking boundary.
pub(crate) struct MemoryRetriever<'db> {
    db: &'db Database,
}

impl<'db> MemoryRetriever<'db> {
    pub(crate) fn new(db: &'db Database) -> Self {
        Self { db }
    }

    /// Read keyword candidates only. This is intentionally separate from
    /// vector retrieval so prompt rendering can merge both candidate pools
    /// without repeating SQL or embedding work.
    pub(crate) fn keyword(&self, query: &MemoryQuery) -> anyhow::Result<Vec<MemoryHit>> {
        let hits = match query.kind {
            kind if kind.storage_kind() == MemoryEntityKind::Fact => {
                let terms = haven_common::text::memory_recall_terms(&query.text);
                let term_refs = haven_common::text::memory_recall_term_sample(&terms, 6);
                let memory_kind = query.kind.fact_kind().map(MemoryFactKind::as_str);
                let facts = self.db.search_facts_any_scoped_by_kind(
                    &term_refs,
                    query.limit.saturating_mul(4),
                    query.fact_subject.as_deref(),
                    memory_kind,
                )?;
                let mut facts = filter_visible_facts(facts);
                facts.truncate(query.limit);
                facts
                    .into_iter()
                    .map(|fact| {
                        let score = fact_effective_confidence(&fact);
                        MemoryHit {
                            entity_id: fact.fact_id,
                            text: format!("{}={}", fact.predicate, fact.object),
                            score,
                            model: String::new(),
                        }
                    })
                    .collect()
            }
            MemoryRecallKind::Episode => self
                .db
                .search_episodes_by_keywords(
                    &haven_common::text::memory_recall_term_sample(
                        &haven_common::text::memory_recall_terms(&query.text),
                        6,
                    ),
                    query.limit.saturating_mul(4),
                    query.exclude_session_id.as_deref(),
                )?
                .into_iter()
                .filter(|hit| !is_sensitive_text(&hit.text))
                .map(|hit| MemoryHit {
                    entity_id: hit.entity_id,
                    text: hit.text,
                    // Preserve the existing wire meaning for keyword episode
                    // hits: only vector recall exposes a similarity score.
                    score: 0.0,
                    model: String::new(),
                })
                .take(query.limit)
                .collect(),
            _ => unreachable!("fact recall kinds share the fact storage branch"),
        };
        Ok(hits)
    }

    /// Read vector candidates from the shared embedding persistence path.
    /// The model is mandatory so mixed embedding spaces cannot leak into a
    /// result set. Fact rows are rehydrated before filtering to protect
    /// against sensitive rows whose embedding text is incomplete.
    pub(crate) fn vector(
        &self,
        query: &MemoryQuery,
        vector: &[f32],
        model: &str,
    ) -> anyhow::Result<Vec<MemoryHit>> {
        if vector.is_empty() || model.trim().is_empty() {
            return Ok(Vec::new());
        }
        let candidate_limit = query.limit.saturating_mul(4);
        let raw = self.db.search_embeddings_filtered_by_memory_kind(
            query.kind.storage_kind().entity_type(),
            vector,
            candidate_limit,
            model,
            MemoryEmbeddingFilter {
                fact_subject: query.fact_subject.as_deref(),
                exclude_session_id: query.exclude_session_id.as_deref(),
                memory_kind: query.kind.fact_kind().map(MemoryFactKind::as_str),
            },
        )?;
        let mut hits: Vec<MemoryHit> = match query.kind {
            kind if kind.storage_kind() == MemoryEntityKind::Fact => {
                let ids: Vec<String> = raw
                    .iter()
                    .map(|(embedding, _)| embedding.entity_id.clone())
                    .collect();
                let facts_by_id: HashMap<String, Fact> = self
                    .db
                    .list_facts_by_ids(&ids)?
                    .into_iter()
                    .map(|fact| (fact.fact_id.clone(), fact))
                    .collect();
                raw.into_iter()
                    .filter_map(|(embedding, score)| {
                        let fact = facts_by_id.get(&embedding.entity_id)?;
                        is_visible_fact(fact).then(|| MemoryHit {
                            entity_id: fact.fact_id.clone(),
                            text: format!("{}={}", fact.predicate, fact.object),
                            score,
                            model: embedding.model,
                        })
                    })
                    .collect()
            }
            MemoryRecallKind::Episode => raw
                .into_iter()
                .filter(|(embedding, _)| !is_sensitive_text(&embedding.text))
                .map(|(embedding, score)| MemoryHit::from_embedding(embedding, score))
                .collect(),
            _ => unreachable!("fact recall kinds share the fact storage branch"),
        };
        hits.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.entity_id.cmp(&b.entity_id))
                .then_with(|| a.text.cmp(&b.text))
        });
        hits.truncate(query.limit);
        Ok(hits)
    }

    /// Combine keyword and vector candidates with reciprocal-rank fusion.
    /// Keyword and vector scores live in different spaces, so comparing their
    /// raw floating-point values is invalid. RRF keeps each source's ranking,
    /// rewards agreement, and remains deterministic across providers.
    pub(crate) fn merge(
        &self,
        query: &MemoryQuery,
        keyword_hits: Vec<MemoryHit>,
        vector_hits: Vec<MemoryHit>,
    ) -> MemoryRecall {
        const RRF_K: f64 = 60.0;
        let has_vectors = !vector_hits.is_empty();
        let has_keyword_hits = !keyword_hits.is_empty();
        let mut candidates: HashMap<String, (MemoryHit, f64)> = HashMap::new();

        for (rank, hit) in vector_hits.into_iter().enumerate() {
            if hit.entity_id.is_empty() {
                continue;
            }
            let key = hit.entity_id.clone();
            let entry = candidates.entry(key).or_insert((hit, 0.0));
            entry.1 += 1.0 / (RRF_K + rank as f64 + 1.0);
        }
        for (rank, hit) in keyword_hits.into_iter().enumerate() {
            if hit.entity_id.is_empty() {
                continue;
            }
            let key = hit.entity_id.clone();
            let entry = candidates.entry(key).or_insert((hit, 0.0));
            entry.1 += 1.0 / (RRF_K + rank as f64 + 1.0);
        }

        let mut ranked: Vec<(MemoryHit, f64)> = candidates.into_values().collect();
        ranked.sort_by(|(left, left_rrf), (right, right_rrf)| {
            right_rrf
                .partial_cmp(left_rrf)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| {
                    right
                        .score
                        .partial_cmp(&left.score)
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
                .then_with(|| left.entity_id.cmp(&right.entity_id))
                .then_with(|| left.text.cmp(&right.text))
        });
        let hits: Vec<MemoryHit> = ranked
            .into_iter()
            .take(query.limit)
            .map(|(hit, _)| hit)
            .collect();
        let empty_reason = hits.is_empty().then_some(MemoryRecallEmptyReason::NoHits);
        let diagnostics = if hits.is_empty() {
            let mut suggestions = vec![
                MemoryRecallSuggestion::BroadenQuery,
                MemoryRecallSuggestion::TryOtherKind,
            ];
            if query.fact_subject.is_some() {
                suggestions.push(MemoryRecallSuggestion::RemoveSubjectFilter);
            }
            if !has_vectors {
                suggestions.push(MemoryRecallSuggestion::ConfigureEmbeddings);
            }
            Some(MemoryRecallDiagnostics {
                keyword: if has_keyword_hits {
                    MemoryRecallSourceStatus::Hit
                } else {
                    MemoryRecallSourceStatus::NoHits
                },
                vector: if has_vectors {
                    MemoryRecallSourceStatus::Hit
                } else {
                    MemoryRecallSourceStatus::NoHits
                },
                suggestions,
            })
        } else {
            None
        };

        MemoryRecall {
            hits,
            mode: if has_vectors {
                MemoryRecallMode::Hybrid
            } else {
                MemoryRecallMode::Keyword
            },
            empty_reason,
            diagnostics,
        }
    }

    /// Full shared retrieval. Vector acquisition is optional and failures are
    /// represented by an empty vector candidate pool, so keyword recall is a
    /// reliable degradation path.
    pub(crate) fn retrieve(
        &self,
        query: &MemoryQuery,
        vector_hits: Option<Vec<MemoryHit>>,
    ) -> anyhow::Result<MemoryRecall> {
        let vector_configured = vector_hits.is_some();
        let mut keyword_hits = self.keyword(query)?;
        let vector_hits = vector_hits.unwrap_or_default();
        if query.kind.storage_kind() == MemoryEntityKind::Fact {
            let mut seed_ids = Vec::new();
            let mut seen_seeds = std::collections::HashSet::new();
            for hit in keyword_hits.iter().chain(vector_hits.iter()) {
                if seen_seeds.insert(hit.entity_id.clone()) {
                    seed_ids.push(hit.entity_id.clone());
                }
                if seed_ids.len() >= MAX_GRAPH_SEED_FACTS {
                    break;
                }
            }
            if !seed_ids.is_empty() {
                let memory_kind = query.kind.fact_kind().map(MemoryFactKind::as_str);
                let related = self.db.related_facts_for_recall(
                    &seed_ids,
                    query.limit.saturating_mul(4),
                    query.fact_subject.as_deref(),
                    memory_kind,
                )?;
                let mut seen_ids: std::collections::HashSet<String> = keyword_hits
                    .iter()
                    .chain(vector_hits.iter())
                    .map(|hit| hit.entity_id.clone())
                    .collect();
                keyword_hits.extend(
                    filter_visible_facts(related)
                        .into_iter()
                        .filter(|fact| seen_ids.insert(fact.fact_id.clone()))
                        .map(|fact| {
                            let score = fact_effective_confidence(&fact);
                            MemoryHit {
                                entity_id: fact.fact_id,
                                text: format!(
                                    "{}: {}={}",
                                    fact.subject, fact.predicate, fact.object
                                ),
                                score,
                                model: String::new(),
                            }
                        }),
                );
            }
        }
        let mut recall = self.merge(query, keyword_hits, vector_hits);
        if recall.hits.is_empty()
            && !vector_configured
            && let Some(diagnostics) = recall.diagnostics.as_mut()
        {
            diagnostics.vector = MemoryRecallSourceStatus::NotConfigured;
        }
        Ok(recall)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::embeddings::entity_kind;

    #[test]
    fn keyword_recall_is_typed_and_filters_sensitive_facts() {
        let db = Database::open_in_memory().unwrap();
        let safe = db
            .insert_fact("user", "likes", "Rust", "user", 0.9, &["preference"])
            .unwrap();
        db.insert_fact("user", "api_key", "super-secret", "user", 1.0, &[])
            .unwrap();

        let query = MemoryQuery::new("Rust", MemoryEntityKind::Fact, 5).unwrap();
        let recall = MemoryRetriever::new(&db).retrieve(&query, None).unwrap();

        assert_eq!(recall.mode, MemoryRecallMode::Keyword);
        assert_eq!(recall.hits.len(), 1);
        assert_eq!(recall.hits[0].entity_id, safe.fact_id);
        assert_eq!(recall.hits[0].text, "likes=Rust");
    }

    #[test]
    fn profile_semantic_and_procedural_recall_filter_facts_by_memory_kind() {
        let db = Database::open_in_memory().unwrap();
        let profile = db
            .set_user_fact("user", "preferred_editor", "Nova", &[])
            .unwrap();
        let semantic = db
            .insert_fact("user", "uses", "Orion", "inferred", 0.9, &[])
            .unwrap();
        let procedural = db
            .set_user_fact_with_kind(
                "user",
                "publish_release_steps",
                "run checks then tag the release",
                &[],
                MemoryFactKind::Procedural,
            )
            .unwrap();

        for (kind, query_text, expected) in [
            (MemoryRecallKind::Profile, "Nova", profile.fact_id.clone()),
            (
                MemoryRecallKind::Semantic,
                "Orion",
                semantic.fact_id.clone(),
            ),
            (
                MemoryRecallKind::Procedural,
                "release",
                procedural.fact_id.clone(),
            ),
        ] {
            let query = MemoryQuery::new(query_text, kind, 5).unwrap();
            let hits = MemoryRetriever::new(&db).keyword(&query).unwrap();
            assert_eq!(hits.len(), 1, "kind={kind:?}");
            assert_eq!(hits[0].entity_id, expected, "kind={kind:?}");
        }

        for fact in [&profile, &semantic, &procedural] {
            db.persist_embedding(
                entity_kind::FACT,
                &fact.fact_id,
                "model-a",
                &[1.0, 0.0],
                "memory",
            )
            .unwrap();
        }
        let query = MemoryQuery::new("memory", MemoryRecallKind::Procedural, 5).unwrap();
        let hits = MemoryRetriever::new(&db)
            .vector(&query, &[1.0, 0.0], "model-a")
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].entity_id, procedural.fact_id);
    }

    #[test]
    fn recall_categories_use_closed_snake_case_values() {
        for (wire, expected) in [
            ("profile", MemoryRecallKind::Profile),
            ("semantic", MemoryRecallKind::Semantic),
            ("procedural", MemoryRecallKind::Procedural),
        ] {
            assert_eq!(MemoryRecallKind::parse(wire).unwrap(), expected);
            assert_eq!(
                serde_json::to_string(&expected).unwrap(),
                format!("\"{wire}\"")
            );
            assert_eq!(
                serde_json::from_str::<MemoryRecallKind>(&format!("\"{wire}\"")).unwrap(),
                expected
            );
        }
        assert!(MemoryRecallKind::parse("all").is_err());
    }

    #[test]
    fn excluded_session_scope_rejects_noncanonical_identity() {
        let query = MemoryQuery::new("recent", MemoryEntityKind::Episode, 5).unwrap();

        let error = query
            .clone()
            .with_excluded_session(Some("ses-short"))
            .unwrap_err();
        assert!(error.to_string().contains("canonical ses-* ID"));

        let session_id = "ses-0123456789abcdef0123456789abcdef";
        assert_eq!(
            query
                .with_excluded_session(Some(session_id))
                .unwrap()
                .excluded_session_id(),
            Some(session_id)
        );
    }

    #[test]
    fn vector_recall_is_typed_hybrid_and_filters_sensitive_rows() {
        let db = Database::open_in_memory().unwrap();
        let safe = db
            .insert_fact("user", "likes", "Rust", "inferred", 0.8, &[])
            .unwrap();
        let secret = db
            .insert_fact("user", "api_key", "super-secret", "inferred", 0.8, &[])
            .unwrap();
        db.persist_embedding(
            entity_kind::FACT,
            &safe.fact_id,
            "model-a",
            &[1.0, 0.0],
            "likes Rust",
        )
        .unwrap();
        db.persist_embedding(
            entity_kind::FACT,
            &secret.fact_id,
            "model-a",
            &[1.0, 0.0],
            "api_key super-secret",
        )
        .unwrap();

        let query = MemoryQuery::new("programming", MemoryEntityKind::Fact, 5).unwrap();
        let vector_hits = MemoryRetriever::new(&db)
            .vector(&query, &[1.0, 0.0], "model-a")
            .unwrap();
        let recall = MemoryRetriever::new(&db)
            .retrieve(&query, Some(vector_hits))
            .unwrap();

        assert_eq!(recall.mode, MemoryRecallMode::Hybrid);
        assert_eq!(recall.hits.len(), 1);
        assert_eq!(recall.hits[0].entity_id, safe.fact_id);
        assert!(
            !recall
                .hits
                .iter()
                .any(|hit| hit.entity_id == secret.fact_id)
        );
    }

    #[test]
    fn empty_vector_degrades_to_keyword_recall() {
        let db = Database::open_in_memory().unwrap();
        let fact = db
            .insert_fact("user", "uses", "SQLite", "user", 1.0, &[])
            .unwrap();
        let query = MemoryQuery::new("SQLite", MemoryEntityKind::Fact, 5).unwrap();
        let recall = MemoryRetriever::new(&db)
            .retrieve(&query, Some(Vec::new()))
            .unwrap();

        assert_eq!(recall.mode, MemoryRecallMode::Keyword);
        assert_eq!(recall.hits[0].entity_id, fact.fact_id);
        assert!(recall.hits[0].model.is_empty());
    }

    #[test]
    fn fact_recall_expands_one_hop_and_filters_sensitive_neighbors() {
        let db = Database::open_in_memory().unwrap();
        let seed = db
            .insert_fact("user", "employer", "Acme", "user", 1.0, &["employer"])
            .unwrap();
        let headquarters = db
            .insert_fact("Acme", "headquartered_in", "Seattle", "inferred", 0.9, &[])
            .unwrap();
        let acquired = db
            .insert_fact("Globex", "acquired", "Acme", "inferred", 0.8, &[])
            .unwrap();
        let secret = db
            .insert_fact("Acme", "api_key", "secret-value", "inferred", 1.0, &[])
            .unwrap();

        let query = MemoryQuery::new("employer", MemoryRecallKind::Fact, 10).unwrap();
        let recall = MemoryRetriever::new(&db).retrieve(&query, None).unwrap();
        let ids: Vec<&str> = recall
            .hits
            .iter()
            .map(|hit| hit.entity_id.as_str())
            .collect();

        assert!(ids.contains(&seed.fact_id.as_str()));
        assert!(ids.contains(&headquarters.fact_id.as_str()));
        assert!(ids.contains(&acquired.fact_id.as_str()));
        assert!(!ids.contains(&secret.fact_id.as_str()));
        assert_eq!(
            recall.hits.len(),
            ids.iter().collect::<std::collections::HashSet<_>>().len()
        );
        assert!(recall.hits.iter().any(|hit| {
            hit.entity_id == headquarters.fact_id && hit.text == "Acme: headquartered_in=Seattle"
        }));
        assert_eq!(recall.hits[0].entity_id, seed.fact_id);
    }

    #[test]
    fn vector_fact_hits_are_graph_expansion_seeds() {
        let db = Database::open_in_memory().unwrap();
        let seed = db
            .insert_fact("user", "works_at", "Acme", "user", 1.0, &[])
            .unwrap();
        let related = db
            .insert_fact("Acme", "headquartered_in", "Seattle", "inferred", 0.9, &[])
            .unwrap();
        let query = MemoryQuery::new("unrelated query", MemoryRecallKind::Fact, 5).unwrap();
        let vector_seed = MemoryHit {
            entity_id: seed.fact_id.clone(),
            text: "works_at=Acme".into(),
            score: 0.9,
            model: "model-a".into(),
        };

        let recall = MemoryRetriever::new(&db)
            .retrieve(&query, Some(vec![vector_seed]))
            .unwrap();

        assert_eq!(recall.mode, MemoryRecallMode::Hybrid);
        assert!(recall.hits.iter().any(|hit| hit.entity_id == seed.fact_id));
        assert!(recall.hits.iter().any(|hit| {
            hit.entity_id == related.fact_id && hit.text == "Acme: headquartered_in=Seattle"
        }));
    }

    #[test]
    fn empty_recall_reports_sources_and_actionable_suggestions() {
        let db = Database::open_in_memory().unwrap();
        let query = MemoryQuery::new("missing", MemoryEntityKind::Fact, 5)
            .unwrap()
            .with_fact_subject(Some("user"));
        let recall = MemoryRetriever::new(&db).retrieve(&query, None).unwrap();

        let diagnostics = recall.diagnostics.unwrap();
        assert_eq!(diagnostics.keyword, MemoryRecallSourceStatus::NoHits);
        assert_eq!(diagnostics.vector, MemoryRecallSourceStatus::NotConfigured);
        assert!(
            diagnostics
                .suggestions
                .contains(&MemoryRecallSuggestion::BroadenQuery)
        );
        assert!(
            diagnostics
                .suggestions
                .contains(&MemoryRecallSuggestion::RemoveSubjectFilter)
        );
        assert!(
            diagnostics
                .suggestions
                .contains(&MemoryRecallSuggestion::ConfigureEmbeddings)
        );
    }

    #[test]
    fn hybrid_recall_rewards_candidates_present_in_both_rankings() {
        let db = Database::open_in_memory().unwrap();
        let query = MemoryQuery::new("anything", MemoryEntityKind::Fact, 3).unwrap();
        let retriever = MemoryRetriever::new(&db);
        let recall = retriever.merge(
            &query,
            vec![
                MemoryHit {
                    entity_id: "keyword-only".into(),
                    text: "keyword".into(),
                    score: 1.0,
                    model: String::new(),
                },
                MemoryHit {
                    entity_id: "agreed".into(),
                    text: "agreed".into(),
                    score: 0.1,
                    model: String::new(),
                },
            ],
            vec![
                MemoryHit {
                    entity_id: "vector-only".into(),
                    text: "vector".into(),
                    score: 1.0,
                    model: "model".into(),
                },
                MemoryHit {
                    entity_id: "agreed".into(),
                    text: "agreed".into(),
                    score: 0.1,
                    model: "model".into(),
                },
            ],
        );

        assert_eq!(recall.mode, MemoryRecallMode::Hybrid);
        assert_eq!(recall.hits[0].entity_id, "agreed");
    }

    #[test]
    fn query_boundary_rejects_unbounded_recall_input() {
        let error = MemoryQuery::new(
            &"x".repeat(MAX_MEMORY_QUERY_CHARS + 1),
            MemoryEntityKind::Fact,
            1,
        )
        .unwrap_err();
        assert!(error.to_string().contains("too long"));
    }

    #[test]
    fn query_boundary_rejects_invalid_kind_and_limit() {
        assert_eq!(
            MemoryEntityKind::parse(" Fact ").unwrap(),
            MemoryEntityKind::Fact
        );
        assert_eq!(
            MemoryEntityKind::parse("EPISODE").unwrap(),
            MemoryEntityKind::Episode
        );
        assert_eq!(MemoryEntityKind::Fact.entity_type(), "fact");
        assert_eq!(MemoryEntityKind::Episode.entity_type(), "episode");
        assert!(MemoryEntityKind::parse("facts").is_err());
        assert!(MemoryQuery::new("query", MemoryEntityKind::Fact, 0).is_err());
        assert!(MemoryQuery::new("query", MemoryEntityKind::Fact, MAX_RECALL_LIMIT + 1).is_err());
        assert_eq!(
            normalize_memory_query("  dark\n\t theme  ").unwrap(),
            "dark theme"
        );
    }

    #[test]
    fn memory_entity_kind_uses_closed_snake_case_serde_values() {
        assert_eq!(
            serde_json::to_string(&MemoryEntityKind::Fact).unwrap(),
            "\"fact\""
        );
        assert_eq!(
            serde_json::to_string(&MemoryEntityKind::Episode).unwrap(),
            "\"episode\""
        );
        assert_eq!(
            serde_json::from_str::<MemoryEntityKind>("\"fact\"").unwrap(),
            MemoryEntityKind::Fact
        );
        assert!(serde_json::from_str::<MemoryEntityKind>("\"all\"").is_err());
        assert!(serde_json::from_str::<MemoryEntityKind>("\"FACT\"").is_err());
    }

    #[test]
    fn fact_subject_scope_is_applied_before_candidate_limit() {
        let db = Database::open_in_memory().unwrap();
        for index in 0..8 {
            db.insert_fact(
                &format!("other-{index}"),
                "likes",
                "needle",
                "user",
                1.0,
                &[],
            )
            .unwrap();
        }
        let target = db
            .insert_fact("target", "likes", "needle", "user", 0.1, &[])
            .unwrap();

        let query = MemoryQuery::new("needle", MemoryEntityKind::Fact, 1)
            .unwrap()
            .with_fact_subject(Some("target"));
        let recall = MemoryRetriever::new(&db).retrieve(&query, None).unwrap();

        assert_eq!(recall.hits.len(), 1);
        assert_eq!(recall.hits[0].entity_id, target.fact_id);
    }

    #[test]
    fn visible_fact_redacts_sensitive_provenance_snippets() {
        let db = Database::open_in_memory().unwrap();
        let source_ref = crate::repositories::facts::FactSourceRef {
            message_id: haven_common::types::new_id("msg"),
            snippet: "password=super-secret".into(),
        };
        db.insert_fact_with_source_ref(
            "user",
            "likes",
            "Rust",
            "inferred",
            0.9,
            &[],
            Some(&source_ref),
            1.0,
        )
        .unwrap();

        let facts = filter_visible_facts(db.list_facts().unwrap());
        assert_eq!(facts.len(), 1);
        assert_eq!(facts[0].source_ref.as_ref().unwrap().snippet, "[redacted]");
    }
}
