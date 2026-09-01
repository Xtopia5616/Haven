//! Read and ranking boundary for memory facts.
//!
//! This module owns fact hydration, list/search queries and the effective
//! confidence ordering shared by prompt recall and UI-facing reads. Mutation
//! rules stay in `fact_graph.rs`; maintenance policies stay in `facts.rs`.

use crate::db::Database;
use chrono::{DateTime, Utc};
use std::collections::HashSet;

use super::facts::{Fact, FactPresence, is_identity_predicate, is_volatile_predicate};

/// Parse a JSON-encoded tag array from a DB string column.
fn parse_tags(tags_str: &str) -> Vec<String> {
    serde_json::from_str(tags_str).unwrap_or_default()
}

/// Canonical SELECT column list for the facts table, in the exact positional
/// order `fact_from_row` maps. Single source of truth: every facts SELECT is
/// built from this const so a column add/remove cannot drift a query away from
/// the row mapper (mirrors `EMBED_COLS` in embeddings.rs).
pub(crate) const FACT_COLS: &str = "id, subject, predicate, object, source, confidence, tags, created_at, mention_count, last_seen_at, provenance_item_id, provenance_record_id, provenance_snippet, durability";

/// Aliased variant for queries that prefix columns with a table alias (FTS
/// join).
pub(crate) const FACT_COLS_ALIASED: &str = "f.id, f.subject, f.predicate, f.object, f.source, f.confidence, f.tags, f.created_at, f.mention_count, f.last_seen_at, f.provenance_item_id, f.provenance_record_id, f.provenance_snippet, f.durability";

/// Map a rusqlite Row with the standard fact SELECT order to a Fact.
pub(crate) fn fact_from_row(row: &rusqlite::Row) -> rusqlite::Result<Fact> {
    let tags_str: String = row.get(6)?;
    let provenance_item_id: Option<String> = row.get(10)?;
    let provenance_record_id: Option<String> = row.get(11)?;
    let provenance_snippet: Option<String> = row.get(12)?;
    Ok(Fact {
        id: row.get(0)?,
        subject: row.get(1)?,
        predicate: row.get(2)?,
        object: row.get(3)?,
        source: row.get(4)?,
        confidence: row.get(5)?,
        tags: parse_tags(&tags_str),
        created_at: row.get(7)?,
        mention_count: row.get(8)?,
        last_seen_at: row.get(9)?,
        source_ref: source_ref_from_provenance(
            provenance_item_id,
            provenance_record_id,
            provenance_snippet,
        ),
        durability: row.get(13)?,
    })
}

fn source_ref_from_provenance(
    item_id: Option<String>,
    record_id: Option<String>,
    snippet: Option<String>,
) -> Option<super::facts::FactSourceRef> {
    let message_id = item_id.or(record_id).unwrap_or_default();
    let snippet = snippet.unwrap_or_default();
    if message_id.is_empty() && snippet.is_empty() {
        None
    } else {
        Some(super::facts::FactSourceRef {
            message_id,
            snippet,
        })
    }
}

/// Parse a fact timestamp. Corrupt rows fall back to now so recency math cannot
/// panic or poison the sort.
fn parse_fact_time(s: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(s)
        .map(|dt| dt.with_timezone(&Utc))
        .unwrap_or_else(|_| Utc::now())
}

/// Age in days based on the latest observation, used by confidence-aware
/// maintenance as well as query ranking.
pub(crate) fn fact_age_days(fact: &Fact) -> f64 {
    let ts = fact.last_seen_at.as_deref().unwrap_or(&fact.created_at);
    (Utc::now() - parse_fact_time(ts)).num_days() as f64
}

/// Effective confidence after recency decay and durability. Identity facts and
/// explicitly user-stated facts never decay; volatile inferred facts decay with
/// a 90-day half-life and other inferred facts with a 365-day half-life.
pub fn fact_effective_confidence(fact: &Fact) -> f64 {
    if is_identity_predicate(&fact.predicate) || fact.source == "user" {
        return fact.confidence;
    }
    let half_life_days = if is_volatile_predicate(&fact.predicate) {
        90.0
    } else {
        365.0
    };
    fact.confidence
        * fact.durability.clamp(0.0, 1.0)
        * 0.5_f64.powf(fact_age_days(fact) / half_life_days)
}

/// Stable sort for prompt/UI display: effective confidence first, then newest
/// last-seen first, then creation time.
fn sort_facts_effective(facts: &mut [Fact]) {
    let scores: Vec<f64> = facts.iter().map(fact_effective_confidence).collect();
    let mut order: Vec<usize> = (0..facts.len()).collect();
    order.sort_by(|&i, &j| {
        scores[j]
            .partial_cmp(&scores[i])
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| {
                facts[j]
                    .last_seen_at
                    .as_deref()
                    .unwrap_or(&facts[j].created_at)
                    .cmp(
                        facts[i]
                            .last_seen_at
                            .as_deref()
                            .unwrap_or(&facts[i].created_at),
                    )
            })
            .then_with(|| facts[j].created_at.cmp(&facts[i].created_at))
    });
    let source = facts.to_vec();
    for (k, &i) in order.iter().enumerate() {
        facts[k] = source[i].clone();
    }
}

impl Database {
    /// Fetch a single fact by id. Used by the prompt builder to resolve vector
    /// recall hits back into full facts for ranking and rendering.
    pub fn get_fact_by_id(&self, id: &str) -> anyhow::Result<Option<Fact>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!(
            "SELECT {FACT_COLS} FROM memory_edges WHERE id = ?1"
        ))?;
        let mut rows = stmt.query(rusqlite::params![id])?;
        match rows.next()? {
            Some(row) => Ok(Some(fact_from_row(row)?)),
            None => Ok(None),
        }
    }

    /// Fetch multiple facts by id in one query. The result is in database row
    /// order; callers that need ranking should use `get_facts`/search APIs.
    pub fn get_facts_by_ids(&self, ids: &[String]) -> anyhow::Result<Vec<Fact>> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let conn = self.conn();
        let placeholders: Vec<String> = (1..=ids.len()).map(|i| format!("?{i}")).collect();
        let sql = format!(
            "SELECT {FACT_COLS} FROM memory_edges WHERE id IN ({})",
            placeholders.join(",")
        );
        let mut stmt = conn.prepare(&sql)?;
        let params = rusqlite::params_from_iter(ids.iter().map(|s| s.as_str()));
        let mut rows = stmt.query(params)?;
        let mut out = Vec::with_capacity(ids.len());
        while let Some(row) = rows.next()? {
            out.push(fact_from_row(row)?);
        }
        Ok(out)
    }

    pub fn get_facts(&self, subject: &str) -> anyhow::Result<Vec<Fact>> {
        if let Some(cached) = self.cache_get_facts(subject) {
            return Ok(cached);
        }
        let key = format!("_facts_{subject}");
        let cache_gen = self.cache_generation(&key);
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!(
            "SELECT {FACT_COLS} FROM memory_edges WHERE subject = ?1"
        ))?;
        let rows = stmt.query_map(rusqlite::params![subject], fact_from_row)?;
        let mut facts = Vec::new();
        for row in rows {
            facts.push(row?);
        }
        sort_facts_effective(&mut facts);
        self.cache_put_facts(subject, facts.clone(), 60, cache_gen);
        Ok(facts)
    }

    /// Seed set for prompt recall: top-`limit` facts for a subject by raw
    /// confidence in SQL, then resorted by effective confidence in Rust.
    pub fn get_facts_limited(&self, subject: &str, limit: usize) -> anyhow::Result<Vec<Fact>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!(
            "SELECT {FACT_COLS} FROM memory_edges WHERE subject = ?1
             ORDER BY confidence DESC, COALESCE(last_seen_at, created_at) DESC
             LIMIT ?2"
        ))?;
        let rows = stmt.query_map(rusqlite::params![subject, limit as i64], fact_from_row)?;
        let mut facts = Vec::new();
        for row in rows {
            facts.push(row?);
        }
        sort_facts_effective(&mut facts);
        Ok(facts)
    }

    /// Batch existence check for fact inference.
    pub fn facts_exist_batch(&self, subjects: &[&str]) -> anyhow::Result<FactPresence> {
        if subjects.is_empty() {
            return Ok((HashSet::new(), HashSet::new()));
        }
        let placeholders = vec!["?"; subjects.len()].join(",");
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!(
            "SELECT subject, predicate, object FROM memory_edges WHERE subject IN ({placeholders})"
        ))?;
        let rows = stmt.query_map(rusqlite::params_from_iter(subjects.iter().copied()), |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?;
        let mut triples = HashSet::new();
        let mut pairs = HashSet::new();
        for row in rows {
            let (subject, predicate, object) = row?;
            triples.insert((subject.clone(), predicate.clone(), object));
            pairs.insert((subject, predicate));
        }
        Ok((triples, pairs))
    }

    /// All facts in effective-confidence order. Cached because this is a
    /// per-extraction hot path; mutations invalidate the generation.
    pub fn list_facts(&self) -> anyhow::Result<Vec<Fact>> {
        if let Some(cached) = self.cache_get_facts_all() {
            return Ok(cached);
        }
        let cache_gen = self.cache_generation("_facts_all");
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!("SELECT {FACT_COLS} FROM memory_edges"))?;
        let rows = stmt.query_map([], fact_from_row)?;
        let mut facts = Vec::new();
        for row in rows {
            facts.push(row?);
        }
        sort_facts_effective(&mut facts);
        self.cache_put_facts_all(facts.clone(), 60, cache_gen);
        Ok(facts)
    }

    pub fn list_facts_by_source(&self, source: &str) -> anyhow::Result<Vec<Fact>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!(
            "SELECT {FACT_COLS} FROM memory_edges WHERE source = ?1"
        ))?;
        let rows = stmt.query_map(rusqlite::params![source], fact_from_row)?;
        let mut facts = Vec::new();
        for row in rows {
            facts.push(row?);
        }
        sort_facts_effective(&mut facts);
        Ok(facts)
    }

    fn build_fts_query(terms: &[&str]) -> String {
        Self::build_fts_query_joined(terms, " AND ")
    }

    fn build_fts_query_or(terms: &[&str]) -> String {
        Self::build_fts_query_joined(terms, " OR ")
    }

    fn build_fts_query_joined(terms: &[&str], sep: &str) -> String {
        terms
            .iter()
            .filter(|t| !t.is_empty())
            .map(|t| format!("\"{}\"", t.replace('"', "\"\"")))
            .collect::<Vec<_>>()
            .join(sep)
    }

    fn escape_like_term(term: &str) -> String {
        let mut out = String::with_capacity(term.len());
        for c in term.chars() {
            match c {
                '\\' | '%' | '_' => {
                    out.push('\\');
                    out.push(c);
                }
                _ => out.push(c),
            }
        }
        out
    }

    fn short_like_terms<'a>(terms: &[&'a str]) -> Vec<&'a str> {
        terms
            .iter()
            .copied()
            .filter(|t| {
                let n = t.chars().count();
                n > 0 && n < 3
            })
            .collect()
    }

    /// Run FTS MATCH; `Ok(None)` means prepare/MATCH failed and the caller may
    /// use LIKE fallback. `Ok(Some(rows))` is a successful query, possibly
    /// empty.
    fn search_facts_fts(
        &self,
        match_expr: &str,
        limit: Option<usize>,
        fact_subject: Option<&str>,
    ) -> anyhow::Result<Option<Vec<Fact>>> {
        let conn = self.conn();
        let edge = crate::embeddings::fts_kind::EDGE;
        let (fts_sql, bind_limit) = if let Some(lim) = limit {
            (
                format!(
                    "SELECT {FACT_COLS_ALIASED}
                     FROM memory_edges f
                     JOIN memory_fts ON memory_fts.entity_id = f.id
                       AND memory_fts.entity_type = '{edge}'
                     WHERE memory_fts MATCH ?1
                       AND (?2 IS NULL OR f.subject = ?2)
                     ORDER BY bm25(memory_fts)
                     LIMIT ?3"
                ),
                Some(lim as i64),
            )
        } else {
            (
                format!(
                    "SELECT {FACT_COLS_ALIASED}
                     FROM memory_edges f
                     JOIN memory_fts ON memory_fts.entity_id = f.id
                       AND memory_fts.entity_type = '{edge}'
                     WHERE memory_fts MATCH ?1
                       AND (?2 IS NULL OR f.subject = ?2)
                     ORDER BY bm25(memory_fts)"
                ),
                None,
            )
        };
        let Ok(mut stmt) = conn.prepare(&fts_sql) else {
            return Ok(None);
        };
        let subject = fact_subject.map(str::to_string);
        let rows = if let Some(lim) = bind_limit {
            stmt.query_map(rusqlite::params![match_expr, subject, lim], fact_from_row)
        } else {
            stmt.query_map(rusqlite::params![match_expr, subject], fact_from_row)
        };
        let Ok(rows) = rows else {
            return Ok(None);
        };
        let mut facts = Vec::new();
        for row in rows {
            match row {
                Ok(f) => facts.push(f),
                Err(_) => return Ok(None),
            }
        }
        Ok(Some(facts))
    }

    fn search_facts_like_any(
        &self,
        terms: &[&str],
        limit: usize,
        fact_subject: Option<&str>,
    ) -> anyhow::Result<Vec<Fact>> {
        if terms.is_empty() || limit == 0 {
            return Ok(Vec::new());
        }
        let conn = self.conn();
        let mut clauses = Vec::with_capacity(terms.len());
        let mut patterns: Vec<String> = Vec::with_capacity(terms.len());
        for (i, term) in terms.iter().enumerate() {
            let p = i + 1;
            clauses.push(format!(
                "(subject LIKE ?{p} ESCAPE '\\' OR predicate LIKE ?{p} ESCAPE '\\' \
                 OR object LIKE ?{p} ESCAPE '\\' OR tags LIKE ?{p} ESCAPE '\\')"
            ));
            patterns.push(format!("%{}%", Self::escape_like_term(term)));
        }
        let subject_param = terms.len() + 1;
        let limit_param = terms.len() + 2;
        let sql = format!(
            "SELECT {FACT_COLS} FROM memory_edges
             WHERE ({}) AND (?{subject_param} IS NULL OR subject = ?{subject_param})
             ORDER BY confidence DESC, COALESCE(last_seen_at, created_at) DESC
             LIMIT ?{limit_param}",
            clauses.join(" OR ")
        );
        let mut stmt = conn.prepare(&sql)?;
        let mut params: Vec<rusqlite::types::Value> = patterns
            .into_iter()
            .map(rusqlite::types::Value::Text)
            .collect();
        params.push(
            fact_subject
                .map(str::to_string)
                .map_or(rusqlite::types::Value::Null, rusqlite::types::Value::Text),
        );
        params.push(rusqlite::types::Value::Integer(limit as i64));
        let mut rows = stmt.query(rusqlite::params_from_iter(params))?;
        let mut facts = Vec::new();
        while let Some(row) = rows.next()? {
            facts.push(fact_from_row(row)?);
        }
        sort_facts_effective(&mut facts);
        Ok(facts)
    }

    fn merge_facts_limited(primary: Vec<Fact>, extra: Vec<Fact>, limit: usize) -> Vec<Fact> {
        if limit == 0 {
            return Vec::new();
        }
        let mut seen: HashSet<String> = HashSet::new();
        let mut out = Vec::with_capacity(limit.min(primary.len() + extra.len()));
        for f in primary.into_iter().chain(extra) {
            if seen.insert(f.id.clone()) {
                out.push(f);
                if out.len() >= limit {
                    break;
                }
            }
        }
        sort_facts_effective(&mut out);
        out
    }

    /// Full-text search across subject, predicate, object and tags. FTS5
    /// trigram is preferred; escaped LIKE handles short terms and unavailable
    /// FTS while preserving the existing long-query miss semantics.
    pub fn search_facts(&self, query: &str) -> anyhow::Result<Vec<Fact>> {
        self.search_facts_scoped(query, None)
    }

    /// Full-text fact search with an optional exact subject scope. The scope is
    /// applied in every SQL branch before its limit, so a noisy subject cannot
    /// crowd out the requested entity's matches.
    pub fn search_facts_scoped(
        &self,
        query: &str,
        fact_subject: Option<&str>,
    ) -> anyhow::Result<Vec<Fact>> {
        let terms: Vec<&str> = query.split_whitespace().collect();
        if terms.is_empty() {
            return Ok(Vec::new());
        }
        let match_expr = Self::build_fts_query(&terms);
        let short = Self::short_like_terms(&terms);
        match self.search_facts_fts(&match_expr, None, fact_subject)? {
            Some(facts) if short.is_empty() => return Ok(facts),
            Some(facts) => {
                let like = self.search_facts_like_any(&short, 50, fact_subject)?;
                return Ok(Self::merge_facts_limited(facts, like, 50));
            }
            None if short.is_empty() => {}
            None => {
                let like_short = self.search_facts_like_any(&short, 50, fact_subject)?;
                if !like_short.is_empty() {
                    return Ok(like_short);
                }
            }
        }
        let pattern = format!("%{}%", Self::escape_like_term(query));
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!(
            "SELECT {FACT_COLS} FROM memory_edges
             WHERE (?2 IS NULL OR subject = ?2)
               AND (subject LIKE ?1 ESCAPE '\\' OR predicate LIKE ?1 ESCAPE '\\'
                OR object LIKE ?1 ESCAPE '\\' OR tags LIKE ?1 ESCAPE '\\')"
        ))?;
        let subject = fact_subject.map(str::to_string);
        let rows = stmt.query_map(rusqlite::params![pattern, subject], fact_from_row)?;
        let mut facts = Vec::new();
        for row in rows {
            facts.push(row?);
        }
        sort_facts_effective(&mut facts);
        Ok(facts)
    }

    /// Multi-term prompt recall: one FTS OR query with SQL LIMIT, unioned with
    /// short-term LIKE hits when trigram cannot index them.
    pub fn search_facts_any(&self, terms: &[&str], limit: usize) -> anyhow::Result<Vec<Fact>> {
        self.search_facts_any_scoped(terms, limit, None)
    }

    /// Multi-term recall with an optional exact subject scope applied before
    /// SQL `LIMIT`. Scoping after a global candidate limit can hide the only
    /// matching fact when another subject has many higher-confidence hits.
    pub fn search_facts_any_scoped(
        &self,
        terms: &[&str],
        limit: usize,
        fact_subject: Option<&str>,
    ) -> anyhow::Result<Vec<Fact>> {
        let terms: Vec<&str> = terms.iter().copied().filter(|t| !t.is_empty()).collect();
        if terms.is_empty() || limit == 0 {
            return Ok(Vec::new());
        }
        let match_expr = Self::build_fts_query_or(&terms);
        let short = Self::short_like_terms(&terms);
        let fts = self.search_facts_fts(&match_expr, Some(limit), fact_subject)?;

        match fts {
            Some(facts) if short.is_empty() => Ok(facts),
            Some(facts) => {
                let like = self.search_facts_like_any(&short, limit, fact_subject)?;
                Ok(Self::merge_facts_limited(facts, like, limit))
            }
            None if short.is_empty() => self.search_facts_like_any(&terms, limit, fact_subject),
            None => {
                let like_short = self.search_facts_like_any(&short, limit, fact_subject)?;
                if like_short.len() >= limit {
                    return Ok(like_short);
                }
                let long: Vec<&str> = terms
                    .iter()
                    .copied()
                    .filter(|t| t.chars().count() >= 3)
                    .collect();
                if long.is_empty() {
                    return Ok(like_short);
                }
                let like_long = self.search_facts_like_any(&long, limit, fact_subject)?;
                Ok(Self::merge_facts_limited(like_short, like_long, limit))
            }
        }
    }

    /// Return all facts that carry the given tag using exact JSON-array
    /// membership rather than a substring scan.
    pub fn get_facts_by_tag(&self, tag: &str) -> anyhow::Result<Vec<Fact>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!(
            "SELECT {FACT_COLS} FROM memory_edges
             WHERE EXISTS (SELECT 1 FROM json_each(memory_edges.tags) AS te WHERE te.value = ?1)"
        ))?;
        let rows = stmt.query_map(rusqlite::params![tag], fact_from_row)?;
        let mut facts = Vec::new();
        for row in rows {
            facts.push(row?);
        }
        sort_facts_effective(&mut facts);
        Ok(facts)
    }
}

#[cfg(test)]
mod tests {
    use super::sort_facts_effective;
    use crate::repositories::facts::Fact;

    fn fact(id: &str, last_seen_at: &str) -> Fact {
        Fact {
            id: id.into(),
            subject: "user".into(),
            predicate: "likes".into(),
            object: id.into(),
            source: "user".into(),
            confidence: 0.8,
            tags: Vec::new(),
            created_at: "2026-01-01T00:00:00+00:00".into(),
            mention_count: 0,
            last_seen_at: Some(last_seen_at.into()),
            source_ref: None,
            durability: 1.0,
        }
    }

    #[test]
    fn effective_sort_uses_latest_observation_as_tie_breaker() {
        let mut facts = vec![
            fact("old", "2026-08-01T00:00:00+00:00"),
            fact("new", "2026-08-02T00:00:00+00:00"),
        ];

        sort_facts_effective(&mut facts);

        assert_eq!(
            facts.iter().map(|f| f.id.as_str()).collect::<Vec<_>>(),
            ["new", "old"]
        );
    }
}
