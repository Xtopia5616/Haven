//! Persistence boundary for maintenance of the memory fact graph.
//!
//! This module owns database-side cleanup and contradiction scanning. It does
//! not decide when maintenance runs or ask an LLM to make a semantic choice;
//! those orchestration and policy decisions remain in `haven-agent`. The
//! public `Database` methods in `facts.rs` are kept as a stable facade for
//! existing callers.

use crate::db::Database;
use chrono::{DateTime, Utc};
use std::collections::{HashMap, HashSet};

use super::fact_query::{FACT_COLS, fact_age_days, fact_effective_confidence, fact_from_row};
use super::facts::{CONTRADICTION_DEMOTE_FACTOR, Fact, all_single_valued_predicates};

/// Live-floor for maintenance contradiction scans (X5). Below this, upsert
/// demotion / flush already treat the fact as inactive in the prompt.
pub const CONTRADICTION_LIVE_FLOOR: f64 = 0.4;

/// Rule-engine demotion age cap (X5). Older losers are not mutated so a
/// maintenance pass cannot push historical edges under the flush floor or
/// rewrite unbounded ancient conflicts on first upgrade.
pub const CONTRADICTION_DEMOTE_MAX_AGE_DAYS: i64 = 2;

/// Kind of contradiction a maintenance candidate group represents (X5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContradictionKind {
    /// `likes` ↔ `dislikes` on the same subject+object.
    Polarity,
    /// Single-valued predicate with multiple distinct objects.
    SingleValued,
}

/// A conflict group surfaced to the optional LLM arbitrator (X5).
#[derive(Debug, Clone)]
pub struct ContradictionCandidate {
    pub kind: ContradictionKind,
    pub facts: Vec<Fact>,
}

/// Internal owner of fact cleanup and maintenance scans.
pub(crate) struct FactMaintenance<'db> {
    db: &'db Database,
}

impl<'db> FactMaintenance<'db> {
    pub(crate) fn new(db: &'db Database) -> Self {
        Self { db }
    }

    /// Distinct predicates with row counts, highest count first (M6).
    pub(crate) fn list_predicate_counts(&self) -> anyhow::Result<Vec<(String, u64)>> {
        let conn = self.db.conn();
        let mut stmt = conn.prepare(
            "SELECT predicate, COUNT(*) AS n FROM memory_edges
             GROUP BY predicate
             ORDER BY n DESC, predicate ASC",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)? as u64))
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// Rewrite every row with predicate `from` to `to`, then collapse exact
    /// duplicates. Returns rows updated before dedup.
    pub(crate) fn rewrite_predicate(&self, from: &str, to: &str) -> anyhow::Result<u64> {
        let from = from.trim();
        let to = to.trim();
        if from.is_empty() || to.is_empty() || from == to {
            return Ok(0);
        }
        let updated = {
            let conn = self.db.conn();
            conn.execute(
                "UPDATE memory_edges SET predicate = ?1 WHERE predicate = ?2",
                rusqlite::params![to, from],
            )? as u64
        };
        if updated > 0 {
            self.db.cache_invalidate_all_facts();
            self.db
                .cache_invalidate_embeddings(crate::embeddings::entity_kind::FACT);
            // Drop the connection before dedup — `conn()` is a mutex pool.
            let _ = self.dedup_facts()?;
        }
        Ok(updated)
    }

    /// Collapse duplicate SPO rows, preserving the highest-confidence row and
    /// merging tags from all rows in each duplicate group.
    pub(crate) fn dedup_facts(&self) -> anyhow::Result<u64> {
        // Only load rows that participate in duplicate groups (not the full
        // table), merge tags onto the keeper, then collapse with the same
        // window DELETE used by migrate_v2.
        let conn = self.db.conn();
        let mut stmt = conn.prepare(&format!(
            "SELECT {FACT_COLS} FROM memory_edges
             WHERE (subject, predicate, object) IN (
                 SELECT subject, predicate, object FROM memory_edges
                 GROUP BY subject, predicate, object
                 HAVING COUNT(*) > 1
             )"
        ))?;
        let rows = stmt.query_map([], fact_from_row)?;
        let mut groups: HashMap<(String, String, String), Vec<Fact>> = HashMap::new();
        for row in rows {
            let fact = row?;
            groups
                .entry((
                    fact.subject.clone(),
                    fact.predicate.clone(),
                    fact.object.clone(),
                ))
                .or_default()
                .push(fact);
        }

        let mut keeper_updates: Vec<(Vec<String>, String)> = Vec::new();
        let had_duplicate_groups = !groups.is_empty();
        for mut group in groups.into_values() {
            if group.len() <= 1 {
                continue;
            }
            group.sort_by(|a, b| {
                b.confidence
                    .partial_cmp(&a.confidence)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then_with(|| b.created_at.cmp(&a.created_at))
            });
            let keeper = group.remove(0);
            let mut tags = keeper.tags.clone();
            for fact in group.iter() {
                for tag in &fact.tags {
                    if !tags.contains(tag) {
                        tags.push(tag.clone());
                    }
                }
            }
            if tags != keeper.tags {
                keeper_updates.push((tags, keeper.id));
            }
        }
        for (tags, id) in &keeper_updates {
            let tags_json = serde_json::to_string(tags).unwrap_or_else(|_| "[]".into());
            conn.execute(
                "UPDATE memory_edges SET tags = ?1 WHERE id = ?2",
                rusqlite::params![tags_json, id],
            )?;
        }

        let deleted = if had_duplicate_groups {
            conn.execute(
                "DELETE FROM memory_edges
                 WHERE id NOT IN (
                     SELECT id FROM (
                         SELECT id, ROW_NUMBER() OVER (
                             PARTITION BY subject, predicate, object
                             ORDER BY confidence DESC, created_at DESC
                         ) AS rn FROM memory_edges
                     ) WHERE rn = 1
                 )",
                [],
            )? as u64
        } else {
            0
        };
        if deleted > 0 || !keeper_updates.is_empty() {
            self.db.cache_invalidate_all_facts();
            self.db
                .cache_invalidate_embeddings(crate::embeddings::entity_kind::FACT);
        }
        Ok(deleted)
    }

    /// Remove facts whose predicate or object looks like a credential. This
    /// is a data purge, not merely a prompt filtering operation.
    pub(crate) fn delete_sensitive_facts(&self) -> anyhow::Result<u64> {
        // Keep this bulk SQL in lockstep with is_sensitive_predicate and
        // is_sensitive_object; the repository boundary tests cover the
        // credential forms that are purged here.
        let conn = self.db.conn();
        let deleted = conn.execute(
            "DELETE FROM memory_edges WHERE
                instr(lower(predicate), 'api_key') > 0
             OR instr(lower(predicate), 'apikey') > 0
             OR instr(lower(predicate), 'api-key') > 0
             OR instr(lower(predicate), 'secret') > 0
             OR instr(lower(predicate), 'token') > 0
             OR instr(lower(predicate), 'password') > 0
             OR instr(lower(predicate), 'passwd') > 0
             OR instr(lower(predicate), 'credential') > 0
             OR instr(lower(predicate), 'passphrase') > 0
             OR instr(lower(predicate), 'access_key') > 0
             OR instr(lower(predicate), 'private_key') > 0
             OR instr(lower(predicate), 'authorization') > 0
             OR lower(trim(object)) LIKE 'sk-%'
             OR lower(trim(object)) LIKE 'tvly-%'
             OR lower(trim(object)) LIKE 'ghp_%'
             OR lower(trim(object)) LIKE 'gho_%'
             OR lower(trim(object)) LIKE 'ghs_%'
             OR lower(trim(object)) LIKE 'github_pat_%'
             OR lower(trim(object)) LIKE 'glpat-%'
             OR lower(trim(object)) LIKE 'xoxb-%'
             OR lower(trim(object)) LIKE 'xoxp-%'
             OR lower(trim(object)) LIKE 'xoxa-%'
             OR lower(trim(object)) LIKE 'xoxr-%'
             OR lower(trim(object)) LIKE 'xapp-%'
             OR lower(trim(object)) LIKE 'npm_%'
             OR lower(trim(object)) LIKE 'pypi-%'
             OR lower(trim(object)) LIKE 'dop_v1_%'
             OR lower(trim(object)) LIKE 'aiza%'
             OR lower(trim(object)) LIKE 'akia%'
             OR lower(trim(object)) LIKE 'asia%'
             OR lower(trim(object)) LIKE 'bearer %'
             OR (lower(trim(object)) LIKE 'eyj%.%.%')
             OR (lower(trim(object)) LIKE '-----begin%' AND instr(lower(trim(object)), 'private key') > 0)
             OR instr(lower(object), 'api_key=') > 0
             OR instr(lower(object), 'apikey=') > 0
             OR (instr(lower(object), '://') > 0
                 AND instr(object, '@') > instr(lower(object), '://'))",
            [],
        )? as u64;
        if deleted > 0 {
            self.db.cache_invalidate_all_facts();
            self.db
                .cache_invalidate_embeddings(crate::embeddings::entity_kind::FACT);
        }
        Ok(deleted)
    }

    /// Remove facts whose effective confidence (after recency decay) is below
    /// the threshold. Fresh facts receive a one-day grace period.
    pub(crate) fn flush_low_confidence(&self, threshold: f64) -> anyhow::Result<u64> {
        // RFC3339 strings sort lexicographically, so SQL filters out the grace
        // window before exact effective-confidence calculation in Rust.
        let cutoff = (Utc::now() - chrono::Duration::days(1)).to_rfc3339();
        let conn = self.db.conn();
        let mut stmt = conn.prepare(&format!(
            "SELECT {FACT_COLS} FROM memory_edges
             WHERE COALESCE(last_seen_at, created_at) <= ?1"
        ))?;
        let rows = stmt.query_map(rusqlite::params![cutoff], fact_from_row)?;
        let mut stale_ids: Vec<String> = Vec::new();
        for row in rows {
            let fact = row?;
            if fact_effective_confidence(&fact) < threshold && fact_age_days(&fact) >= 1.0 {
                stale_ids.push(fact.id);
            }
        }
        if stale_ids.is_empty() {
            return Ok(0);
        }
        let placeholders = vec!["?"; stale_ids.len()].join(",");
        let count = conn.execute(
            &format!("DELETE FROM memory_edges WHERE id IN ({placeholders})"),
            rusqlite::params_from_iter(stale_ids.iter().map(|s| s.as_str())),
        )? as u64;
        self.db.cache_invalidate_all_facts();
        self.db
            .cache_invalidate_embeddings(crate::embeddings::entity_kind::FACT);
        Ok(count)
    }

    /// Normalize empty provenance_record_id strings. Opaque transcript ids
    /// remain untouched; item provenance is handled by the FK.
    pub(crate) fn cleanup_orphan_source_refs(&self) -> anyhow::Result<u64> {
        let conn = self.db.conn();
        let n = conn.execute(
            "UPDATE memory_edges
             SET provenance_record_id = NULL
             WHERE provenance_record_id IS NOT NULL
               AND TRIM(provenance_record_id) = ''",
            [],
        )? as u64;
        if n > 0 {
            self.db.cache_invalidate_all_facts();
        }
        Ok(n)
    }

    /// Scan and demote recent contradiction losers while leaving old rows as
    /// evidence for optional LLM arbitration.
    pub(crate) fn resolve_contradictions(&self) -> anyhow::Result<u64> {
        let now = Utc::now();
        let mut demote_ids: HashSet<String> = HashSet::new();
        for group in self.collect_contradiction_groups()? {
            let Some((_, losers)) = pick_contradiction_keeper(group.kind, &group.facts) else {
                continue;
            };
            for loser in losers {
                if fact_effective_confidence(loser) >= CONTRADICTION_LIVE_FLOOR
                    && fact_within_demote_age(loser, now)
                {
                    demote_ids.insert(loser.id.clone());
                }
            }
        }
        self.demote_fact_ids(demote_ids.into_iter().collect())
    }

    /// Return residual live conflict groups for optional LLM arbitration.
    pub(crate) fn list_ambiguous_contradictions(
        &self,
    ) -> anyhow::Result<Vec<ContradictionCandidate>> {
        let mut out = Vec::new();
        for mut group in self.collect_contradiction_groups()? {
            group
                .facts
                .retain(|fact| fact_effective_confidence(fact) >= CONTRADICTION_LIVE_FLOOR);
            if group.facts.len() >= 2 {
                out.push(group);
            }
        }
        Ok(out)
    }

    /// Scale confidence for the ids selected by a gated maintenance caller.
    pub(crate) fn demote_fact_ids(&self, ids: Vec<String>) -> anyhow::Result<u64> {
        self.demote_fact_ids_by_factor(ids, CONTRADICTION_DEMOTE_FACTOR)
    }

    fn demote_fact_ids_by_factor(&self, ids: Vec<String>, factor: f64) -> anyhow::Result<u64> {
        if ids.is_empty() {
            return Ok(0);
        }
        let factor = factor.clamp(0.0, 1.0);
        let conn = self.db.conn();
        let placeholders = vec!["?"; ids.len()].join(",");
        // Params: factor first, then ids (all anonymous `?` binders).
        let mut params: Vec<rusqlite::types::Value> = Vec::with_capacity(ids.len() + 1);
        params.push(factor.into());
        for id in &ids {
            params.push(id.clone().into());
        }
        let n = conn.execute(
            &format!(
                "UPDATE memory_edges SET confidence = confidence * ? WHERE id IN ({placeholders})"
            ),
            rusqlite::params_from_iter(params),
        )? as u64;
        if n > 0 {
            self.db.cache_invalidate_all_facts();
            self.db
                .cache_invalidate_embeddings(crate::embeddings::entity_kind::FACT);
        }
        Ok(n)
    }

    fn collect_contradiction_groups(&self) -> anyhow::Result<Vec<ContradictionCandidate>> {
        let mut groups = Vec::new();
        groups.extend(self.scan_polarity_contradictions()?);
        groups.extend(self.scan_single_valued_contradictions()?);
        Ok(groups)
    }

    fn scan_polarity_contradictions(&self) -> anyhow::Result<Vec<ContradictionCandidate>> {
        let conn = self.db.conn();
        // Case-insensitive object match so "Rust" / "rust" still conflict.
        // `a.id < b.id` keeps each pair once; hydrate both ids in one IN query.
        let mut stmt = conn.prepare(
            "SELECT a.id, b.id FROM memory_edges a
             INNER JOIN memory_edges b
               ON a.subject = b.subject
              AND lower(a.object) = lower(b.object)
              AND a.id < b.id
             WHERE ((a.predicate = 'likes' AND b.predicate = 'dislikes')
                 OR (a.predicate = 'dislikes' AND b.predicate = 'likes'))
               AND a.confidence >= ?1 AND b.confidence >= ?1",
        )?;
        let pairs: Vec<(String, String)> = stmt
            .query_map(rusqlite::params![CONTRADICTION_LIVE_FLOOR], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        drop(stmt);
        drop(conn);

        if pairs.is_empty() {
            return Ok(Vec::new());
        }
        let mut unique_ids: Vec<String> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        for (a, b) in &pairs {
            if seen.insert(a.clone()) {
                unique_ids.push(a.clone());
            }
            if seen.insert(b.clone()) {
                unique_ids.push(b.clone());
            }
        }
        let by_id: HashMap<String, Fact> = self
            .db
            .get_facts_by_ids(&unique_ids)?
            .into_iter()
            .map(|fact| (fact.id.clone(), fact))
            .collect();
        let mut out = Vec::new();
        for (id_a, id_b) in pairs {
            let Some(a) = by_id.get(&id_a) else {
                continue;
            };
            let Some(b) = by_id.get(&id_b) else {
                continue;
            };
            out.push(ContradictionCandidate {
                kind: ContradictionKind::Polarity,
                facts: vec![a.clone(), b.clone()],
            });
        }
        Ok(out)
    }

    fn scan_single_valued_contradictions(&self) -> anyhow::Result<Vec<ContradictionCandidate>> {
        let predicates: Vec<&str> = all_single_valued_predicates().collect();
        let conn = self.db.conn();
        let placeholders = vec!["?"; predicates.len()].join(",");
        // One scan: all live single-valued rows, then group in Rust where
        // distinct objects collide (avoids N+1 prepare per subject/predicate).
        let sql = format!(
            "SELECT {FACT_COLS} FROM memory_edges
             WHERE lower(predicate) IN ({placeholders})
               AND confidence >= ?"
        );
        let mut stmt = conn.prepare(&sql)?;
        let mut params: Vec<rusqlite::types::Value> = predicates
            .iter()
            .map(|predicate| (*predicate).to_string().into())
            .collect();
        params.push(CONTRADICTION_LIVE_FLOOR.into());
        let rows = stmt.query_map(rusqlite::params_from_iter(params), fact_from_row)?;
        let mut by_key: HashMap<(String, String), Vec<Fact>> = HashMap::new();
        for row in rows {
            let fact = row?;
            let key = (fact.subject.clone(), fact.predicate.to_ascii_lowercase());
            by_key.entry(key).or_default().push(fact);
        }
        drop(stmt);
        drop(conn);

        let mut out = Vec::new();
        for facts in by_key.into_values() {
            let distinct_objects: HashSet<String> = facts
                .iter()
                .map(|fact| fact.object.to_ascii_lowercase())
                .collect();
            if distinct_objects.len() >= 2 && facts.len() >= 2 {
                out.push(ContradictionCandidate {
                    kind: ContradictionKind::SingleValued,
                    facts,
                });
            }
        }
        Ok(out)
    }
}

/// True when the fact was last seen (or created) within the X5 demote age cap.
pub fn fact_within_demote_age(fact: &Fact, now: DateTime<Utc>) -> bool {
    let timestamp = fact.last_seen_at.as_deref().unwrap_or(&fact.created_at);
    match DateTime::parse_from_rfc3339(timestamp) {
        Ok(dt) => {
            now.signed_duration_since(dt.with_timezone(&Utc))
                <= chrono::Duration::days(CONTRADICTION_DEMOTE_MAX_AGE_DAYS)
        }
        // Unparseable timestamps: skip demotion rather than risk mutating
        // opaque historical rows into the flush window.
        Err(_) => false,
    }
}

/// Pick the keeper and losers for a conflict group.
///
/// - **Polarity**: user > inferred, then newest observation, confidence and
///   mentions.
/// - **Single-valued**: user > inferred, then effective confidence, mentions,
///   and recency.
pub fn pick_contradiction_keeper(
    kind: ContradictionKind,
    facts: &[Fact],
) -> Option<(&Fact, Vec<&Fact>)> {
    if facts.len() < 2 {
        return None;
    }
    let mut order: Vec<usize> = (0..facts.len()).collect();
    order.sort_by(|&i, &j| contradiction_cmp(kind, &facts[j], &facts[i]));
    let keeper = &facts[order[0]];
    let losers: Vec<&Fact> = order[1..].iter().map(|&i| &facts[i]).collect();
    Some((keeper, losers))
}

fn fact_recency_key(fact: &Fact) -> &str {
    fact.last_seen_at.as_deref().unwrap_or(&fact.created_at)
}

fn contradiction_cmp(kind: ContradictionKind, a: &Fact, b: &Fact) -> std::cmp::Ordering {
    let user_ord = (a.source == "user").cmp(&(b.source == "user"));
    if user_ord != std::cmp::Ordering::Equal {
        return user_ord;
    }
    match kind {
        ContradictionKind::Polarity => fact_recency_key(a)
            .cmp(fact_recency_key(b))
            .then_with(|| {
                fact_effective_confidence(a)
                    .partial_cmp(&fact_effective_confidence(b))
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .then_with(|| a.mention_count.cmp(&b.mention_count)),
        ContradictionKind::SingleValued => fact_effective_confidence(a)
            .partial_cmp(&fact_effective_confidence(b))
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.mention_count.cmp(&b.mention_count))
            .then_with(|| fact_recency_key(a).cmp(fact_recency_key(b))),
    }
}

#[cfg(test)]
mod tests {
    use super::{ContradictionKind, FactMaintenance};
    use crate::Database;

    #[test]
    fn maintenance_facade_deduplicates_and_merges_tags() {
        let db = Database::open_in_memory().unwrap();
        db.insert_fact("user", "likes", "Rust", "inferred", 0.5, &["preference"])
            .unwrap();
        db.insert_fact("user", "likes", "Rust", "inferred", 0.9, &["workspace"])
            .unwrap();

        let deleted = FactMaintenance::new(&db).dedup_facts().unwrap();

        assert_eq!(deleted, 1);
        let facts = db.get_facts("user").unwrap();
        assert_eq!(facts.len(), 1);
        assert_eq!(facts[0].confidence, 0.9);
        assert!(facts[0].tags.contains(&"preference".to_string()));
        assert!(facts[0].tags.contains(&"workspace".to_string()));
    }

    #[test]
    fn maintenance_sensitive_cleanup_preserves_non_sensitive_fact() {
        let db = Database::open_in_memory().unwrap();
        db.insert_fact("user", "likes", "Rust", "user", 1.0, &[])
            .unwrap();
        db.insert_fact("user", "notes", "sk-do-not-store", "inferred", 1.0, &[])
            .unwrap();

        let deleted = FactMaintenance::new(&db).delete_sensitive_facts().unwrap();

        assert_eq!(deleted, 1);
        let facts = db.list_facts().unwrap();
        assert_eq!(facts.len(), 1);
        assert_eq!(facts[0].object, "Rust");
    }

    #[test]
    fn maintenance_contradiction_scan_returns_candidate_kind() {
        let db = Database::open_in_memory().unwrap();
        db.insert_fact("user", "likes", "Rust", "inferred", 0.9, &[])
            .unwrap();
        db.insert_fact("user", "dislikes", "Rust", "user", 1.0, &[])
            .unwrap();

        let groups = FactMaintenance::new(&db)
            .list_ambiguous_contradictions()
            .unwrap();

        assert!(
            groups.iter().any(|group| {
                group.kind == ContradictionKind::Polarity && group.facts.len() == 2
            })
        );
    }
}
