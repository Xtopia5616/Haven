use crate::db::Database;
use crate::repositories::messages::Message;
use chrono::{DateTime, NaiveDateTime, Utc};
use std::collections::HashMap;
use uuid::Uuid;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Fact {
    pub id: String,
    pub subject: String,
    pub predicate: String,
    pub object: String,
    pub source: String,
    pub confidence: f64,
    pub tags: Vec<String>,
    pub created_at: String,
    /// How many times this fact has been re-confirmed (reinforcement).
    #[serde(default)]
    pub mention_count: i64,
    /// RFC3339 timestamp of the last time this fact was observed.
    #[serde(default)]
    pub last_seen_at: Option<String>,
    /// The conversation message this fact was extracted from, if known.
    #[serde(default)]
    pub source_ref: Option<FactSourceRef>,
}

/// Reference back to the conversation message a fact was extracted from.
/// Stored as a JSON object in the `source_ref` column for traceability and
/// contradiction checks.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct FactSourceRef {
    pub message_id: String,
    /// Short excerpt (≤ 120 chars) of the message that supported the fact.
    pub snippet: String,
}

impl FactSourceRef {
    pub fn from_message(message_id: &str, content: &str) -> Self {
        let snippet: String = content.chars().take(120).collect();
        Self {
            message_id: message_id.into(),
            snippet,
        }
    }
}

/// A fact extracted by the rule-based inference engine, before it is
/// persisted to the database.
#[derive(Debug, Clone)]
pub struct InferredFact {
    pub subject: String,
    pub predicate: String,
    pub object: String,
    pub confidence: f64,
    pub tags: Vec<String>,
    pub source_ref: Option<FactSourceRef>,
}

/// What `upsert_fact` did with an extracted fact.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpsertOutcome {
    /// A brand-new fact was stored.
    Inserted,
    /// The identical triple already existed; it was reinforced.
    Reinforced,
    /// A single-valued predicate changed; the old inferred values were demoted
    /// and the new fact stored.
    Corrected,
    /// The incoming fact was dropped: a user-stated value already exists for
    /// this single-valued predicate, and user-stated values are authoritative.
    Skipped,
}

/// Parse a JSON-encoded tag array from a DB string column.
fn parse_tags(tags_str: &str) -> Vec<String> {
    serde_json::from_str(tags_str).unwrap_or_default()
}

/// Serialize a slice of tag strings into a JSON array string for storage.
fn serialize_tags(tags: &[&str]) -> String {
    serde_json::to_string(tags).unwrap_or_else(|_| "[]".into())
}

/// Map a rusqlite Row (with the standard 11-column SELECT order) to a Fact.
/// Shared by all query methods to avoid drift when columns change.
fn fact_from_row(row: &rusqlite::Row) -> rusqlite::Result<Fact> {
    let tags_str: String = row.get(6)?;
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
        source_ref: parse_source_ref(row.get::<_, Option<String>>(10)?),
    })
}

/// Parse a JSON-encoded source reference from a DB string column.
fn parse_source_ref(raw: Option<String>) -> Option<FactSourceRef> {
    raw.and_then(|v| serde_json::from_str(&v).ok())
}

/// Serialize a source reference into its JSON string column form.
fn serialize_source_ref(source_ref: Option<&FactSourceRef>) -> Option<String> {
    source_ref.and_then(|r| serde_json::to_string(r).ok())
}

/// Predicates describing stable identity attributes: they never decay and
/// never get auto-corrected away by a contradicting inference.
pub fn is_identity_predicate(predicate: &str) -> bool {
    matches!(
        predicate.to_ascii_lowercase().as_str(),
        "name" | "birthday" | "email" | "phone" | "city" | "country" | "timezone"
    )
}

/// Predicates that change over time (paths, employers, tooling): these decay
/// fastest so stale values drop out of the prompt once they are no longer
/// confirmed.
pub fn is_volatile_predicate(predicate: &str) -> bool {
    matches!(
        predicate.to_ascii_lowercase().as_str(),
        "project_path" | "works_at" | "uses" | "workspace"
    )
}

/// Predicates that hold a single value per subject at any point in time. When
/// a new fact with the same predicate but a different object is extracted, the
/// old inferred values are demoted instead of coexisting (a new project path
/// supersedes the old one; a user can like both Rust and Go and use several
/// tools at once though).
pub fn is_single_valued_predicate(predicate: &str) -> bool {
    is_identity_predicate(predicate)
        || matches!(
            predicate.to_ascii_lowercase().as_str(),
            "project_path"
                | "works_at"
                | "workspace"
                | "company"
                | "job"
                | "role"
                | "shell"
                | "os"
                | "location"
                | "address"
        )
}

/// Parse a fact timestamp that may be RFC3339 (inserted via code) or the
/// SQLite `datetime('now')` format (legacy rows).
fn parse_fact_time(s: &str) -> DateTime<Utc> {
    if let Ok(dt) = DateTime::parse_from_rfc3339(s) {
        return dt.with_timezone(&Utc);
    }
    if let Ok(naive) = NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S") {
        return DateTime::from_naive_utc_and_offset(naive, Utc);
    }
    Utc::now()
}

fn fact_age_days(fact: &Fact) -> f64 {
    let ts = fact.last_seen_at.as_deref().unwrap_or(&fact.created_at);
    (Utc::now() - parse_fact_time(ts)).num_days() as f64
}

/// Effective confidence after recency decay. Identity facts never decay, and
/// neither do explicitly user-stated facts (`source="user"`, e.g. added via
/// the settings UI — the user can remove those, time should not). Volatile
/// facts decay with a 90-day half-life; everything else (preferences etc.)
/// with a 365-day half-life. Old inferred facts that stop being re-confirmed
/// sink below the flush threshold and get pruned, while freshly confirmed
/// facts keep their full weight.
pub fn fact_effective_confidence(fact: &Fact) -> f64 {
    if is_identity_predicate(&fact.predicate) || fact.source == "user" {
        return fact.confidence;
    }
    let half_life_days = if is_volatile_predicate(&fact.predicate) {
        90.0
    } else {
        365.0
    };
    fact.confidence * 0.5_f64.powf(fact_age_days(fact) / half_life_days)
}

/// Stable sort for prompt/UI display: effective confidence first, then newest
/// last-seen first, then creation time.
fn sort_facts_effective(facts: &mut [Fact]) {
    // Precompute each fact's effective confidence once so the comparator does
    // not re-parse timestamps / read the clock for every comparison.
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

/// Map a predicate to a default tag set used by the inference engine.
fn tags_for_predicate(predicate: &str) -> Vec<String> {
    match predicate {
        "likes" | "dislikes" | "uses" => vec!["preference".into()],
        "project_path" => vec!["workspace".into()],
        "name" | "works_at" => vec!["identity".into()],
        _ => vec![],
    }
}

/// Predicate names that must never be stored as (or shown from) user facts:
/// API keys, tokens, passwords and other credentials.
pub fn is_sensitive_predicate(predicate: &str) -> bool {
    let p = predicate.to_ascii_lowercase();
    const SENSITIVE_KEYWORDS: &[&str] = &[
        "api_key",
        "apikey",
        "api-key",
        "secret",
        "token",
        "password",
        "passwd",
        "credential",
        "passphrase",
        "access_key",
        "private_key",
        "authorization",
    ];
    SENSITIVE_KEYWORDS.iter().any(|k| p.contains(k))
}

/// Object values that look like credentials even when the predicate is not
/// obviously sensitive (defense in depth: covers secrets the LLM happened to
/// store under an innocent predicate).
pub fn is_sensitive_object(object: &str) -> bool {
    let o = object.trim().to_ascii_lowercase();
    o.starts_with("sk-")
        || o.starts_with("tvly-")
        || o.starts_with("ghp_")
        || o.starts_with("gho_")
        || o.starts_with("xoxb-")
        || o.starts_with("aiza")
        || o.starts_with("bearer ")
        || o.contains("api_key=")
        || o.contains("apikey=")
}

impl Database {
    pub fn insert_fact(
        &self,
        subject: &str,
        predicate: &str,
        object: &str,
        source: &str,
        confidence: f64,
        tags: &[&str],
    ) -> anyhow::Result<Fact> {
        self.insert_fact_with_source_ref(subject, predicate, object, source, confidence, tags, None)
    }

    /// Insert a fact with an optional reference to the message it came from.
    #[allow(clippy::too_many_arguments)]
    pub fn insert_fact_with_source_ref(
        &self,
        subject: &str,
        predicate: &str,
        object: &str,
        source: &str,
        confidence: f64,
        tags: &[&str],
        source_ref: Option<&FactSourceRef>,
    ) -> anyhow::Result<Fact> {
        let id = Uuid::new_v4().to_string();
        let now = Utc::now().to_rfc3339();
        let tags_json = serialize_tags(tags);
        let source_ref_json = serialize_source_ref(source_ref);
        let conn = self.conn();
        conn.execute(
            "INSERT INTO facts (id, subject, predicate, object, source, confidence, created_at, tags, mention_count, last_seen_at, source_ref)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 0, ?9, ?10)",
            rusqlite::params![
                id,
                subject,
                predicate,
                object,
                source,
                confidence,
                now,
                tags_json,
                now,
                source_ref_json
            ],
        )?;
        self.cache_invalidate_facts(subject);
        Ok(Fact {
            id,
            subject: subject.into(),
            predicate: predicate.into(),
            object: object.into(),
            source: source.into(),
            confidence,
            tags: tags.iter().map(|s| s.to_string()).collect(),
            created_at: now.clone(),
            mention_count: 0,
            last_seen_at: Some(now),
            source_ref: source_ref.cloned(),
        })
    }

    /// Insert a fact only if the same (subject, predicate, object) triple
    /// does not already exist. Returns the existing fact when present, so
    /// repeated startup seeding never accumulates duplicates.
    pub fn ensure_fact(
        &self,
        subject: &str,
        predicate: &str,
        object: &str,
        source: &str,
        confidence: f64,
        tags: &[&str],
    ) -> anyhow::Result<Fact> {
        let existing: Option<Fact> = {
            let conn = self.conn();
            conn.query_row(
                "SELECT id, subject, predicate, object, source, confidence, tags, created_at, mention_count, last_seen_at, source_ref
                 FROM facts WHERE subject = ?1 AND predicate = ?2 AND object = ?3",
                rusqlite::params![subject, predicate, object],
                fact_from_row,
            )
            .ok()
        };
        if let Some(existing) = existing {
            return Ok(existing);
        }
        self.insert_fact(subject, predicate, object, source, confidence, tags)
    }

    /// Insert, reinforce, or correct a fact extracted from a conversation.
    ///
    /// - Same (subject, predicate, object) triple already present →
    ///   reinforcement: bump `mention_count`, refresh `last_seen_at`, and
    ///   raise confidence toward the incoming value.
    /// - Same predicate, different object, and the predicate is single-valued
    ///   (e.g. `project_path`) → correction: demote the old inferred facts of
    ///   that predicate, then insert the new one. When a user-stated value
    ///   already exists for that predicate, an incoming inferred value is
    ///   dropped entirely ([`UpsertOutcome::Skipped`]) — user facts win.
    /// - Otherwise → plain insert.
    ///
    /// `source_ref` points at the supporting conversation message; on
    /// reinforcement it replaces the stored reference when provided.
    #[allow(clippy::too_many_arguments)]
    pub fn upsert_fact(
        &self,
        subject: &str,
        predicate: &str,
        object: &str,
        source: &str,
        confidence: f64,
        tags: &[&str],
        source_ref: Option<&FactSourceRef>,
    ) -> anyhow::Result<UpsertOutcome> {
        let now = Utc::now().to_rfc3339();
        let mut corrected = false;
        {
            let conn = self.conn();
            let existing: Option<Fact> = conn
                .query_row(
                    "SELECT id, subject, predicate, object, source, confidence, tags, created_at, mention_count, last_seen_at, source_ref
                     FROM facts WHERE subject = ?1 AND predicate = ?2 AND object = ?3",
                    rusqlite::params![subject, predicate, object],
                    fact_from_row,
                )
                .ok();
            if let Some(existing) = existing {
                // Reinforcement: repeated confirmation keeps a fact alive and
                // nudges its confidence up (capped at 1.0, never below incoming).
                let boosted = (existing.confidence * 1.05).min(1.0).max(confidence);
                let merged_ref = source_ref.or(existing.source_ref.as_ref());
                // Merge any newly attached tags into the stored set so a
                // re-extraction that re-tags a fact does not lose the tag.
                let mut merged_tags = existing.tags.clone();
                for t in tags {
                    if !merged_tags.iter().any(|x| x == t) {
                        merged_tags.push((*t).to_string());
                    }
                }
                let tag_refs: Vec<&str> = merged_tags.iter().map(|s| s.as_str()).collect();
                conn.execute(
                    "UPDATE facts
                     SET mention_count = mention_count + 1, last_seen_at = ?1, confidence = ?2,
                         source_ref = ?3, tags = ?4
                     WHERE id = ?5",
                    rusqlite::params![
                        now,
                        boosted,
                        serialize_source_ref(merged_ref),
                        serialize_tags(&tag_refs),
                        existing.id
                    ],
                )?;
                self.cache_invalidate_facts(subject);
                return Ok(UpsertOutcome::Reinforced);
            }

            if is_single_valued_predicate(predicate) {
                // A user-stated value is authoritative: never let inference
                // store a contradicting inferred value alongside it.
                let has_user_value = conn
                    .query_row(
                        "SELECT 1 FROM facts WHERE subject = ?1 AND predicate = ?2 AND source = 'user' AND object <> ?3 LIMIT 1",
                        rusqlite::params![subject, predicate, object],
                        |r| r.get::<_, i32>(0),
                    )
                    .map(|_| true)
                    .unwrap_or(false);
                if has_user_value && source == "inferred" {
                    return Ok(UpsertOutcome::Skipped);
                }
                let n = conn.execute(
                    "UPDATE facts SET confidence = confidence * 0.5
                     WHERE subject = ?1 AND predicate = ?2 AND object <> ?3 AND source = 'inferred'",
                    rusqlite::params![subject, predicate, object],
                )?;
                corrected = n > 0;
            }

            // §P2: polarity conflict — "likes X" and "dislikes X" contradict
            // each other; the newest observation demotes the opposite-polarity
            // fact so the prompt never shows both. User-stated facts always
            // win: an inferred fact never demotes a user-stated opposite.
            let opposite = match predicate.to_ascii_lowercase().as_str() {
                "likes" => Some("dislikes"),
                "dislikes" => Some("likes"),
                _ => None,
            };
            if let Some(opp) = opposite {
                let incoming_is_user = (source == "user") as i32;
                let _ = conn.execute(
                    "UPDATE facts SET confidence = confidence * 0.5
                     WHERE subject = ?1 AND object = ?2 AND predicate = ?3
                       AND (?4 = 1 OR source = 'inferred')",
                    rusqlite::params![subject, object, opp, incoming_is_user],
                )?;
            }
        }
        let _ = self.insert_fact_with_source_ref(
            subject, predicate, object, source, confidence, tags, source_ref,
        )?;
        Ok(if corrected {
            UpsertOutcome::Corrected
        } else {
            UpsertOutcome::Inserted
        })
    }

    pub fn get_facts(&self, subject: &str) -> anyhow::Result<Vec<Fact>> {
        if let Some(cached) = self.cache_get_facts(subject) {
            return Ok(cached);
        }
        let key = format!("_facts_{}", subject);
        let cache_gen = self.cache_generation(&key);
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT id, subject, predicate, object, source, confidence, tags, created_at, mention_count, last_seen_at, source_ref
             FROM facts WHERE subject = ?1",
        )?;
        let rows = stmt.query_map(rusqlite::params![subject], fact_from_row)?;
        let mut facts = Vec::new();
        for row in rows {
            facts.push(row?);
        }
        sort_facts_effective(&mut facts);
        self.cache_put_facts(subject, facts.clone(), 60, cache_gen);
        Ok(facts)
    }

    pub fn list_facts(&self) -> anyhow::Result<Vec<Fact>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT id, subject, predicate, object, source, confidence, tags, created_at, mention_count, last_seen_at, source_ref
             FROM facts",
        )?;
        let rows = stmt.query_map([], fact_from_row)?;
        let mut facts = Vec::new();
        for row in rows {
            facts.push(row?);
        }
        sort_facts_effective(&mut facts);
        Ok(facts)
    }

    pub fn list_facts_by_source(&self, source: &str) -> anyhow::Result<Vec<Fact>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT id, subject, predicate, object, source, confidence, tags, created_at, mention_count, last_seen_at, source_ref
             FROM facts WHERE source = ?1",
        )?;
        let rows = stmt.query_map(rusqlite::params![source], fact_from_row)?;
        let mut facts = Vec::new();
        for row in rows {
            facts.push(row?);
        }
        sort_facts_effective(&mut facts);
        Ok(facts)
    }

    /// Build a safe FTS5 MATCH expression from a free-text query: each
    /// whitespace-separated term is quoted (quotes doubled) and AND-combined,
    /// so arbitrary user input cannot smuggle FTS operators into the query.
    fn build_fts_query(terms: &[&str]) -> String {
        terms
            .iter()
            .filter(|t| !t.is_empty())
            .map(|t| format!("\"{}\"", t.replace('"', "\"\"")))
            .collect::<Vec<_>>()
            .join(" AND ")
    }

    /// Full-text search across subject, predicate, object, and tags. Uses the
    /// FTS5 index (BM25 relevance ranking) when available; falls back to the
    /// old LIKE substring scan when the index is missing or the query fails.
    pub fn search_facts(&self, query: &str) -> anyhow::Result<Vec<Fact>> {
        let terms: Vec<&str> = query.split_whitespace().collect();
        if terms.is_empty() {
            return Ok(Vec::new());
        }
        let match_expr = Self::build_fts_query(&terms);
        let conn = self.conn();
        let fts_sql = "SELECT f.id, f.subject, f.predicate, f.object, f.source, f.confidence, f.tags, f.created_at, f.mention_count, f.last_seen_at, f.source_ref
                       FROM facts f
                       JOIN facts_fts ON f.rowid = facts_fts.rowid
                       WHERE facts_fts MATCH ?1
                       ORDER BY bm25(facts_fts)";
        if let Ok(mut stmt) = conn.prepare(fts_sql)
            && let Ok(rows) = stmt.query_map(rusqlite::params![match_expr], fact_from_row)
        {
            let mut facts = Vec::new();
            let mut valid = true;
            for row in rows {
                match row {
                    Ok(f) => facts.push(f),
                    Err(_) => {
                        // Invalid MATCH expression (e.g. stray quote) — fall
                        // back to substring search below.
                        valid = false;
                        break;
                    }
                }
            }
            // A valid FTS query may still return zero rows: FTS5's default
            // tokenizer does not split CJK runs or do substring matching, so
            // fall through to the LIKE scan on empty results to preserve the
            // old substring behavior.
            if valid && !facts.is_empty() {
                return Ok(facts);
            }
        }
        let pattern = format!("%{}%", query);
        let mut stmt = conn.prepare(
            "SELECT id, subject, predicate, object, source, confidence, tags, created_at, mention_count, last_seen_at, source_ref
             FROM facts
             WHERE subject LIKE ?1 OR predicate LIKE ?1 OR object LIKE ?1 OR tags LIKE ?1",
        )?;
        let rows = stmt.query_map(rusqlite::params![pattern], fact_from_row)?;
        let mut facts = Vec::new();
        for row in rows {
            facts.push(row?);
        }
        sort_facts_effective(&mut facts);
        Ok(facts)
    }

    /// Return all facts that carry the given tag.
    ///
    /// Uses `json_each` (exact JSON-array membership) rather than a `LIKE
    /// '%"tag"%'` substring scan, which could false-positive on adjacent tags
    /// (`"preferences"` matching a search for `"preference"` or tags whose
    /// text contains the quoted needle).
    pub fn get_facts_by_tag(&self, tag: &str) -> anyhow::Result<Vec<Fact>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT id, subject, predicate, object, source, confidence, tags, created_at, mention_count, last_seen_at, source_ref
             FROM facts
             WHERE EXISTS (SELECT 1 FROM json_each(facts.tags) AS te WHERE te.value = ?1)",
        )?;
        let rows = stmt.query_map(rusqlite::params![tag], fact_from_row)?;
        let mut facts = Vec::new();
        for row in rows {
            facts.push(row?);
        }
        sort_facts_effective(&mut facts);
        Ok(facts)
    }

    pub fn delete_fact(&self, id: &str) -> anyhow::Result<()> {
        let conn = self.conn();
        // Query subject before deletion so we can invalidate the right cache.
        let subject: Option<String> = conn
            .query_row(
                "SELECT subject FROM facts WHERE id = ?1",
                rusqlite::params![id],
                |r| r.get(0),
            )
            .ok();
        conn.execute("DELETE FROM facts WHERE id = ?1", rusqlite::params![id])?;
        if let Some(s) = subject {
            self.cache_invalidate_facts(&s);
        }
        Ok(())
    }

    pub fn dedup_facts(&self) -> anyhow::Result<u64> {
        // Group by (subject, predicate, object) regardless of tags: the same
        // triple is the same fact even when older rows carry a different (or
        // empty) tag set. The previous tag-sensitive grouping let repeated
        // re-extraction pile up duplicates — e.g. 379 rows of `name=Xtopia`.
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT id, subject, predicate, object, source, confidence, tags, created_at, mention_count, last_seen_at, source_ref
             FROM facts",
        )?;
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

        // Tag merge: collect tag updates for keepers whose duplicates carried
        // extra tags, and the ids of every duplicate row to delete. The keeper
        // rule lives in ONE place (the Rust sort below) so the merge target is
        // always the same row the delete keeps.
        let mut keeper_updates: Vec<(Vec<String>, String)> = Vec::new();
        let mut duplicate_ids: Vec<String> = Vec::new();
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
                duplicate_ids.push(fact.id.clone());
                for t in &fact.tags {
                    if !tags.contains(t) {
                        tags.push(t.clone());
                    }
                }
            }
            if tags != keeper.tags {
                keeper_updates.push((tags, keeper.id));
            }
        }

        // Bulk delete the collected duplicate ids in one statement.
        let deleted = if duplicate_ids.is_empty() {
            0
        } else {
            let placeholders = vec!["?"; duplicate_ids.len()].join(",");
            conn.execute(
                &format!("DELETE FROM facts WHERE id IN ({placeholders})"),
                rusqlite::params_from_iter(duplicate_ids.iter().map(|s| s.as_str())),
            )? as u64
        };
        for (tags, id) in keeper_updates {
            let tag_refs: Vec<&str> = tags.iter().map(|s| s.as_str()).collect();
            conn.execute(
                "UPDATE facts SET tags = ?1 WHERE id = ?2",
                rusqlite::params![serialize_tags(&tag_refs), id],
            )?;
        }
        self.cache_invalidate_facts("user");
        Ok(deleted)
    }

    /// Remove facts whose predicate or object looks like a credential. Called
    /// during fact maintenance so secrets accidentally extracted in the past
    /// are purged from the database rather than merely hidden from prompts.
    pub fn delete_sensitive_facts(&self) -> anyhow::Result<u64> {
        let facts = self.list_facts()?;
        let mut deleted: u64 = 0;
        for fact in &facts {
            if is_sensitive_predicate(&fact.predicate) || is_sensitive_object(&fact.object) {
                let conn = self.conn();
                conn.execute(
                    "DELETE FROM facts WHERE id = ?1",
                    rusqlite::params![fact.id],
                )?;
                deleted += 1;
            }
        }
        if deleted > 0 {
            self.cache_invalidate_facts("user");
        }
        Ok(deleted)
    }

    /// Remove facts whose effective confidence (after recency decay) is below
    /// the threshold. Stale volatile facts that stopped being re-confirmed
    /// sink below the bar and are pruned; freshly confirmed or identity facts
    /// keep their weight.
    pub fn flush_low_confidence(&self, threshold: f64) -> anyhow::Result<u64> {
        let facts = self.list_facts()?;
        let mut stale_ids: Vec<String> = Vec::new();
        for fact in &facts {
            if fact_effective_confidence(fact) < threshold {
                stale_ids.push(fact.id.clone());
            }
        }
        if stale_ids.is_empty() {
            return Ok(0);
        }
        // Batch the deletion in a single statement instead of one DELETE per
        // stale row (each row delete re-acquired the connection and fired the
        // FTS trigger).
        let placeholders = vec!["?"; stale_ids.len()].join(",");
        let conn = self.conn();
        let count = conn.execute(
            &format!("DELETE FROM facts WHERE id IN ({placeholders})"),
            rusqlite::params_from_iter(stale_ids.iter().map(|s| s.as_str())),
        )? as u64;
        self.cache_invalidate_facts("user");
        Ok(count)
    }

    /// Extract and clean the object that follows a trigger phrase at byte
    /// offset `from` in the ORIGINAL-cased message. Splits at sentence
    /// punctuation, strips trailing fluff phrases (preference rules only),
    /// caps the length, and rejects degenerate values (empty / too short).
    /// Returns `None` when nothing usable follows the phrase.
    fn extract_object(content_orig: &str, from: usize, strip_fluff: bool) -> Option<String> {
        let mut obj = content_orig[from..]
            .split(['.', ',', '!', '?', ';', '\n'])
            .next()
            .unwrap_or("")
            .trim()
            .to_string();
        // Strip trailing punctuation that survived the sentence split.
        while let Some(c) = obj.chars().last() {
            if matches!(c, '.' | ',' | '!' | '?' | ';' | ':') {
                obj.pop();
            } else {
                break;
            }
        }
        if strip_fluff {
            // Repeated confirmation fillers inflate preference objects
            // ("I like Rust programming very much."). Strip the trailing
            // phrase instead of storing it as part of the liked thing.
            let mut changed = true;
            while changed {
                changed = false;
                for phrase in [
                    " very much",
                    " a lot",
                    " as well",
                    " all the time",
                    " at all",
                    " too",
                    " now",
                    " actually",
                ] {
                    if obj.to_lowercase().ends_with(phrase) {
                        obj.truncate(obj.len() - phrase.len());
                        changed = true;
                    }
                }
            }
            obj = obj.trim().to_string();
        }
        if obj.chars().count() < 2 || obj.chars().count() > 80 {
            return None;
        }
        Some(obj)
    }

    /// Extract a filesystem path that follows a trigger phrase. Unlike
    /// [`Self::extract_object`], the object is cut at the path itself: the
    /// first `/` or `\` (optionally preceded by a drive letter like `C:`) is
    /// the start, and the first whitespace after it is the end — so
    /// "my project at /home/user/app works fine." yields `/home/user/app`
    /// instead of capturing the whole sentence. When no path separator is
    /// present (e.g. "my workspace is here") it falls back to the general
    /// object extraction.
    fn extract_path_object(content_orig: &str, from: usize) -> Option<String> {
        let raw = content_orig[from..]
            .split(['.', ',', '!', '?', ';', '\n'])
            .next()
            .unwrap_or("")
            .trim();
        if raw.is_empty() {
            return None;
        }
        // No path separator: fall back to the general object extraction
        // ("my workspace is tidy" still yields a usable object).
        let sep_idx = match raw.find(['/', '\\']) {
            Some(i) => i,
            None => return Self::extract_object(content_orig, from, false),
        };
        // Keep the drive letter when the separator follows one ("D:\...").
        let start = if sep_idx >= 2
            && raw.as_bytes()[sep_idx - 1] == b':'
            && raw.as_bytes()[sep_idx - 2].is_ascii_alphanumeric()
        {
            sep_idx - 2
        } else {
            sep_idx
        };
        let path = raw[start..].split_whitespace().next().unwrap_or("");
        if path.chars().count() < 2 {
            return None;
        }
        Some(path.to_string())
    }

    pub fn infer_facts_from_messages(&self, messages: &[Message]) -> Vec<InferredFact> {
        let mut facts: Vec<InferredFact> = Vec::new();
        let mut corrected_predicates: Vec<String> = Vec::new();

        for msg in messages {
            let content = msg.content.to_lowercase();
            let content_orig = &msg.content;
            // Every rule-built fact points back at the message it came from.
            let source_ref = || Some(FactSourceRef::from_message(&msg.id, &msg.content));

            // Rule 1: "I like/love/prefer X" -> ("user", "likes", "X", 0.9)
            for pattern in &["i like ", "i love ", "i prefer ", "my favorite "] {
                if let Some(idx) = content.find(pattern)
                    && let Some(obj) = Self::extract_object(content_orig, idx + pattern.len(), true)
                {
                    facts.push(InferredFact {
                        subject: "user".into(),
                        predicate: "likes".into(),
                        object: obj,
                        confidence: 0.9,
                        tags: tags_for_predicate("likes"),
                        source_ref: source_ref(),
                    });
                }
            }

            // Rule 2: "I don't like / I hate X" -> ("user", "dislikes", "X", 0.8)
            for pattern in &["i don't like ", "i hate ", "i dislike "] {
                if let Some(idx) = content.find(pattern)
                    && let Some(obj) = Self::extract_object(content_orig, idx + pattern.len(), true)
                {
                    facts.push(InferredFact {
                        subject: "user".into(),
                        predicate: "dislikes".into(),
                        object: obj,
                        confidence: 0.8,
                        tags: tags_for_predicate("dislikes"),
                        source_ref: source_ref(),
                    });
                }
            }

            // Rule 3: "my project at /path" -> ("user", "project_path", "/path", 0.7)
            let path_patterns = [
                "my project at ",
                "my project is at ",
                "my project is in ",
                "my code is at ",
                "my workspace is ",
            ];
            for pattern in &path_patterns {
                if let Some(idx) = content.find(pattern)
                    && let Some(obj) = Self::extract_path_object(content_orig, idx + pattern.len())
                {
                    facts.push(InferredFact {
                        subject: "user".into(),
                        predicate: "project_path".into(),
                        object: obj,
                        confidence: 0.7,
                        tags: tags_for_predicate("project_path"),
                        source_ref: source_ref(),
                    });
                }
            }

            // Rule 4: "actually I prefer Y" -> correction: lower confidence of existing facts with same predicate
            if content.contains("actually")
                && (content.contains("prefer")
                    || content.contains("use")
                    || content.contains("want"))
            {
                corrected_predicates.push("likes".into());
            }

            // Rule 5: "my name is X" or "I am X" -> ("user", "name", "X", 0.85)
            for pattern in &["my name is ", "i am ", "call me ", "i'm ", "i am called "] {
                if let Some(idx) = content.find(pattern) {
                    let after = &content[idx + pattern.len()..];
                    let stop_words = ["looking", "trying", "going", "using", "working", "doing"];
                    let first_word = after.split_whitespace().next().unwrap_or("");
                    if !stop_words.contains(&first_word) {
                        // Names are short: cap at 4 words, no fluff strip.
                        let obj = Self::extract_object(content_orig, idx + pattern.len(), false)
                            .filter(|o| o.split_whitespace().count() <= 4);
                        if let Some(obj) = obj {
                            facts.push(InferredFact {
                                subject: "user".into(),
                                predicate: "name".into(),
                                object: obj,
                                confidence: 0.85,
                                tags: tags_for_predicate("name"),
                                source_ref: source_ref(),
                            });
                        }
                    }
                }
            }

            // Rule 6: "I use X" -> ("user", "uses", "X", 0.7)
            if let Some(idx) = content.find("i use ")
                && let Some(obj) = Self::extract_object(content_orig, idx + 6, false)
            {
                facts.push(InferredFact {
                    subject: "user".into(),
                    predicate: "uses".into(),
                    object: obj,
                    confidence: 0.7,
                    tags: tags_for_predicate("uses"),
                    source_ref: source_ref(),
                });
            }

            // Rule 7: "I work at/for X" -> ("user", "works_at", "X", 0.75)
            for pattern in &["i work at ", "i work for ", "i work in "] {
                if let Some(idx) = content.find(pattern)
                    && let Some(obj) =
                        Self::extract_object(content_orig, idx + pattern.len(), false)
                {
                    facts.push(InferredFact {
                        subject: "user".into(),
                        predicate: "works_at".into(),
                        object: obj,
                        confidence: 0.75,
                        tags: tags_for_predicate("works_at"),
                        source_ref: source_ref(),
                    });
                }
            }
        }

        // Lower confidence of corrected facts
        if !corrected_predicates.is_empty() {
            let conn = self.conn();
            for pred in &corrected_predicates {
                let _ = conn.execute(
                    "UPDATE facts SET confidence = confidence * 0.5 WHERE subject = 'user' AND predicate = ?1 AND source = 'inferred'",
                    rusqlite::params![pred],
                );
            }
        }

        facts
    }
}

#[cfg(test)]
mod tests {
    use super::{FactSourceRef, UpsertOutcome, fact_effective_confidence};
    use crate::Database;

    fn create_db() -> Database {
        Database::open_in_memory().unwrap()
    }

    fn make_message(content: &str) -> crate::repositories::messages::Message {
        crate::repositories::messages::Message {
            id: uuid::Uuid::new_v4().to_string(),
            task_id: "t1".into(),
            role: "user".into(),
            content: content.into(),
            message_type: Some("text".into()),
            created_at: "2026-01-01T00:00:00Z".into(),
            tool_call_id: None,
            is_compacted: false,
            compaction_id: None,
            parent_message_id: None,
            attachments: vec![],
            voice: false,
        }
    }

    #[test]
    fn test_insert_fact() {
        let db = create_db();
        let fact = db
            .insert_fact("user", "likes", "Rust", "user", 0.9, &["preference"])
            .unwrap();
        assert!(!fact.id.is_empty());
        assert_eq!(fact.subject, "user");
        assert_eq!(fact.predicate, "likes");
        assert_eq!(fact.object, "Rust");
        assert_eq!(fact.source, "user");
        assert_eq!(fact.confidence, 0.9);
        assert_eq!(fact.tags, vec!["preference"]);
        assert!(!fact.created_at.is_empty());
    }

    #[test]
    fn test_insert_fact_no_tags() {
        let db = create_db();
        let fact = db
            .insert_fact("user", "custom", "some value", "user", 0.5, &[])
            .unwrap();
        assert!(fact.tags.is_empty());
    }

    #[test]
    fn test_get_facts_by_subject() {
        let db = create_db();
        db.insert_fact("user", "likes", "Rust", "user", 0.9, &["preference"])
            .unwrap();
        db.insert_fact("user", "likes", "Python", "user", 0.7, &["preference"])
            .unwrap();
        db.insert_fact("other", "likes", "Go", "user", 0.5, &[])
            .unwrap();

        let user_facts = db.get_facts("user").unwrap();
        assert_eq!(user_facts.len(), 2);
        assert_eq!(user_facts[0].object, "Rust");
        assert_eq!(user_facts[1].object, "Python");
        assert_eq!(user_facts[0].tags, vec!["preference"]);
    }

    #[test]
    fn test_get_facts_ordering() {
        let db = create_db();
        db.insert_fact("user", "likes", "A", "user", 0.5, &[])
            .unwrap();
        db.insert_fact("user", "likes", "B", "user", 0.9, &[])
            .unwrap();
        db.insert_fact("user", "likes", "C", "user", 0.7, &[])
            .unwrap();

        let facts = db.get_facts("user").unwrap();
        assert_eq!(facts.len(), 3);
        assert!(facts[0].confidence >= facts[1].confidence);
        assert!(facts[1].confidence >= facts[2].confidence);
    }

    #[test]
    fn test_list_facts() {
        let db = create_db();
        db.insert_fact("user", "likes", "Rust", "user", 0.9, &["preference"])
            .unwrap();
        db.insert_fact("other", "dislikes", "Java", "user", 0.5, &[])
            .unwrap();

        let all = db.list_facts().unwrap();
        assert_eq!(all.len(), 2);
        assert!(all[0].confidence >= all[1].confidence);
    }

    #[test]
    fn test_list_facts_by_source() {
        let db = create_db();
        db.insert_fact("user", "likes", "Rust", "user", 0.9, &[])
            .unwrap();
        db.insert_fact("user", "likes", "Python", "inferred", 0.7, &[])
            .unwrap();

        let user_sourced = db.list_facts_by_source("user").unwrap();
        assert_eq!(user_sourced.len(), 1);
        assert_eq!(user_sourced[0].object, "Rust");

        let inferred = db.list_facts_by_source("inferred").unwrap();
        assert_eq!(inferred.len(), 1);
        assert_eq!(inferred[0].object, "Python");

        let none = db.list_facts_by_source("unknown").unwrap();
        assert!(none.is_empty());
    }

    #[test]
    fn test_search_facts() {
        let db = create_db();
        db.insert_fact(
            "user",
            "likes",
            "Rust programming",
            "user",
            0.9,
            &["preference"],
        )
        .unwrap();
        db.insert_fact("user", "dislikes", "Java", "user", 0.5, &["preference"])
            .unwrap();
        db.insert_fact(
            "user",
            "uses",
            "TypeScript",
            "inferred",
            0.7,
            &["preference"],
        )
        .unwrap();

        let results = db.search_facts("programming").unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].object, "Rust programming");

        let results = db.search_facts("Rust").unwrap();
        assert_eq!(results.len(), 1);

        let results = db.search_facts("Java").unwrap();
        assert_eq!(results.len(), 1);

        let results = db.search_facts("nonexistent").unwrap();
        assert!(results.is_empty());

        // FTS5 token matching: "likes" matches the likes predicate but not
        // the substring inside "dislikes".
        let results = db.search_facts("likes").unwrap();
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn test_search_facts_by_tag() {
        let db = create_db();
        db.insert_fact("user", "name", "Alice", "user", 1.0, &["identity"])
            .unwrap();
        db.insert_fact("user", "likes", "Rust", "user", 0.9, &["preference"])
            .unwrap();
        db.insert_fact(
            "user",
            "project_path",
            "/home/app",
            "user",
            0.8,
            &["workspace"],
        )
        .unwrap();

        let results = db.search_facts("preference").unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].object, "Rust");

        let results = db.search_facts("identity").unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].object, "Alice");

        let results = db.search_facts("workspace").unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].object, "/home/app");
    }

    #[test]
    fn test_get_facts_by_tag() {
        let db = create_db();
        db.insert_fact("user", "name", "Alice", "user", 1.0, &["identity"])
            .unwrap();
        db.insert_fact("user", "works_at", "Acme", "user", 0.8, &["identity"])
            .unwrap();
        db.insert_fact("user", "likes", "Rust", "user", 0.9, &["preference"])
            .unwrap();

        let results = db.get_facts_by_tag("identity").unwrap();
        assert_eq!(results.len(), 2);

        let results = db.get_facts_by_tag("preference").unwrap();
        assert_eq!(results.len(), 1);

        let results = db.get_facts_by_tag("nonexistent").unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn test_get_facts_by_tag_partial_match_excluded() {
        let db = create_db();
        db.insert_fact("user", "likes", "Rust", "user", 0.9, &["preference"])
            .unwrap();
        db.insert_fact("user", "likes", "Go", "user", 0.8, &["preferences"])
            .unwrap();

        // "preference" should NOT match "preferences" — tag matching is exact.
        let results = db.get_facts_by_tag("preference").unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].object, "Rust");
    }

    #[test]
    fn test_delete_fact_existing() {
        let db = create_db();
        let fact = db
            .insert_fact("user", "likes", "Rust", "user", 0.9, &[])
            .unwrap();
        db.delete_fact(&fact.id).unwrap();
        let remaining = db.get_facts("user").unwrap();
        assert!(remaining.is_empty());
    }

    #[test]
    fn test_delete_fact_non_existing() {
        let db = create_db();
        let result = db.delete_fact("non-existent-id");
        assert!(result.is_ok());
    }

    #[test]
    fn test_dedup_facts_keeps_one_per_triple() {
        let db = create_db();
        db.insert_fact("user", "likes", "Rust", "user", 0.5, &[])
            .unwrap();
        db.insert_fact("user", "likes", "Rust", "user", 0.9, &[])
            .unwrap();
        db.insert_fact("user", "likes", "Rust", "user", 0.3, &[])
            .unwrap();
        db.insert_fact("user", "dislikes", "Java", "user", 0.7, &[])
            .unwrap();

        let count = db.dedup_facts().unwrap();
        assert!(count > 0);

        let remaining = db.list_facts().unwrap();
        assert_eq!(remaining.len(), 2);
        let rust_facts: Vec<_> = remaining.iter().filter(|f| f.object == "Rust").collect();
        assert_eq!(rust_facts.len(), 1);
    }

    #[test]
    fn test_dedup_facts_keeps_highest_confidence() {
        let db = create_db();
        db.insert_fact("user", "likes", "Rust", "user", 0.3, &[])
            .unwrap();
        db.insert_fact("user", "likes", "Rust", "user", 0.9, &[])
            .unwrap();
        db.insert_fact("user", "likes", "Rust", "user", 0.5, &[])
            .unwrap();

        db.dedup_facts().unwrap();

        let remaining = db.get_facts("user").unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(
            remaining[0].confidence, 0.9,
            "should keep highest confidence"
        );
    }

    #[test]
    fn test_dedup_facts_merges_different_tags() {
        let db = create_db();
        db.insert_fact("user", "likes", "Rust", "user", 0.9, &["preference"])
            .unwrap();
        db.insert_fact("user", "likes", "Rust", "user", 0.7, &["workspace"])
            .unwrap();

        let count = db.dedup_facts().unwrap();
        assert!(count > 0, "same triple with different tags must dedup");
        let remaining = db.list_facts().unwrap();
        assert_eq!(remaining.len(), 1);
        // Tags are merged, not dropped.
        assert!(remaining[0].tags.contains(&"preference".to_string()));
        assert!(remaining[0].tags.contains(&"workspace".to_string()));
        assert_eq!(
            remaining[0].confidence, 0.9,
            "keeper keeps highest confidence"
        );
    }

    #[test]
    fn test_ensure_fact_idempotent() {
        let db = create_db();
        db.ensure_fact("user", "name", "Xtopia", "user", 1.0, &["identity"])
            .unwrap();
        db.ensure_fact("user", "name", "Xtopia", "user", 1.0, &["identity"])
            .unwrap();
        db.ensure_fact("user", "name", "Xtopia", "user", 1.0, &["identity"])
            .unwrap();
        let facts = db.get_facts("user").unwrap();
        assert_eq!(facts.len(), 1, "ensure_fact must not accumulate duplicates");
    }

    #[test]
    fn test_delete_sensitive_facts() {
        let db = create_db();
        db.insert_fact("user", "name", "Alice", "user", 1.0, &["identity"])
            .unwrap();
        db.insert_fact("user", "likes", "Rust", "user", 0.9, &["preference"])
            .unwrap();
        db.insert_fact(
            "user",
            "tavily_api_key",
            "tvly-dev-abc",
            "inferred",
            1.0,
            &["workspace"],
        )
        .unwrap();
        db.insert_fact(
            "user",
            "secret_token",
            "ghp_xxxx",
            "inferred",
            1.0,
            &["workspace"],
        )
        .unwrap();

        let deleted = db.delete_sensitive_facts().unwrap();
        assert_eq!(deleted, 2);
        let remaining = db.list_facts().unwrap();
        assert_eq!(remaining.len(), 2);
        assert!(
            remaining
                .iter()
                .all(|f| f.predicate != "tavily_api_key" && f.predicate != "secret_token")
        );
    }

    #[test]
    fn test_dedup_facts_no_duplicates() {
        let db = create_db();
        db.insert_fact("user", "likes", "Rust", "user", 0.9, &[])
            .unwrap();
        db.insert_fact("user", "dislikes", "Java", "user", 0.5, &[])
            .unwrap();

        let count = db.dedup_facts().unwrap();
        assert_eq!(count, 0);
        let remaining = db.list_facts().unwrap();
        assert_eq!(remaining.len(), 2);
    }

    #[test]
    fn test_flush_low_confidence() {
        let db = create_db();
        db.insert_fact("user", "likes", "Rust", "user", 0.9, &[])
            .unwrap();
        db.insert_fact("user", "likes", "Python", "user", 0.5, &[])
            .unwrap();
        db.insert_fact("user", "dislikes", "Java", "inferred", 0.3, &[])
            .unwrap();

        let count = db.flush_low_confidence(0.6).unwrap();
        assert_eq!(count, 2);
        let remaining = db.list_facts().unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].object, "Rust");
    }

    #[test]
    fn test_flush_low_confidence_none_below() {
        let db = create_db();
        db.insert_fact("user", "likes", "Rust", "user", 0.9, &[])
            .unwrap();

        let count = db.flush_low_confidence(0.5).unwrap();
        assert_eq!(count, 0);
        let remaining = db.list_facts().unwrap();
        assert_eq!(remaining.len(), 1);
    }

    #[test]
    fn test_flush_low_confidence_all_below() {
        let db = create_db();
        db.insert_fact("user", "likes", "A", "inferred", 0.1, &[])
            .unwrap();
        db.insert_fact("user", "likes", "B", "inferred", 0.2, &[])
            .unwrap();

        let count = db.flush_low_confidence(1.0).unwrap();
        assert_eq!(count, 2);
        let remaining = db.list_facts().unwrap();
        assert!(remaining.is_empty());
    }

    #[test]
    fn test_infer_rule_likes() {
        let db = create_db();
        let msgs = vec![make_message("I like Rust programming very much.")];
        let facts = db.infer_facts_from_messages(&msgs);
        assert_eq!(facts.len(), 1);
        assert_eq!(facts[0].subject, "user");
        assert_eq!(facts[0].predicate, "likes");
        assert_eq!(facts[0].object, "Rust programming");
        assert_eq!(facts[0].confidence, 0.9);
        assert_eq!(facts[0].tags, vec!["preference"]);
    }

    #[test]
    fn test_infer_rule_likes_strips_fluff_phrases() {
        let db = create_db();
        let msgs = vec![
            make_message("I like dark themes a lot."),
            make_message("I love Rust too."),
            make_message("I prefer concise answers as well."),
            make_message("I like plain text."),
        ];
        let facts = db.infer_facts_from_messages(&msgs);
        assert_eq!(facts.len(), 4);
        assert_eq!(facts[0].object, "dark themes");
        assert_eq!(facts[1].object, "Rust");
        assert_eq!(facts[2].object, "concise answers");
        assert_eq!(facts[3].object, "plain text");
    }

    #[test]
    fn test_infer_rule_rejects_degenerate_objects() {
        let db = create_db();
        // Two-char minimum: "I hate ." and "I like x." (1-char object) are
        // both rejected; only the 2+ char object survives.
        let msgs = vec![
            make_message("I hate ."),
            make_message("I like x."),
            make_message("I like Go."),
        ];
        let facts = db.infer_facts_from_messages(&msgs);
        assert_eq!(facts.len(), 1);
        assert_eq!(facts[0].predicate, "likes");
        assert_eq!(facts[0].object, "Go");
    }

    #[test]
    fn test_infer_rule_likes_variants() {
        let db = create_db();
        let msgs = vec![
            make_message("I love TypeScript."),
            make_message("I prefer dark themes."),
            make_message("My favorite language is Go."),
        ];
        let facts = db.infer_facts_from_messages(&msgs);
        assert_eq!(facts.len(), 3);
        for f in &facts {
            assert_eq!(f.predicate, "likes");
            assert_eq!(f.confidence, 0.9);
            assert_eq!(f.tags, vec!["preference"]);
        }
    }

    #[test]
    fn test_infer_rule_dislikes() {
        let db = create_db();
        let msgs = vec![
            make_message("I don't like Java at all."),
            make_message("I hate slow builds."),
            make_message("I dislike complicated configs."),
        ];
        let facts = db.infer_facts_from_messages(&msgs);
        assert_eq!(facts.len(), 3);
        for f in &facts {
            assert_eq!(f.predicate, "dislikes");
            assert_eq!(f.confidence, 0.8);
            assert_eq!(f.tags, vec!["preference"]);
        }
    }

    #[test]
    fn test_infer_rule_project_path() {
        let db = create_db();
        let msgs = vec![
            make_message("my project at /home/user/myapp works fine."),
            make_message("my project is at /workspace/backend"),
            make_message("my code is at /Users/dev/src"),
        ];
        let facts = db.infer_facts_from_messages(&msgs);
        assert_eq!(facts.len(), 3);
        for f in &facts {
            assert_eq!(f.predicate, "project_path");
            assert_eq!(f.confidence, 0.7);
            assert_eq!(f.tags, vec!["workspace"]);
        }
        assert_eq!(facts[0].object, "/home/user/myapp");
        assert_eq!(facts[1].object, "/workspace/backend");
        assert_eq!(facts[2].object, "/Users/dev/src");
    }

    #[test]
    fn test_infer_rule_project_path_windows_and_preposition() {
        let db = create_db();
        let msgs = vec![
            // Windows drive path keeps the drive letter.
            make_message("my workspace is D:\\Workspace\\Haven"),
            // A stray preposition before the path is not captured.
            make_message("my project is at /opt/tools and it works"),
            // No path separator: falls back to the general object rule.
            make_message("my workspace is tidy"),
        ];
        let facts = db.infer_facts_from_messages(&msgs);
        let objects: Vec<&str> = facts.iter().map(|f| f.object.as_str()).collect();
        assert!(objects.contains(&"D:\\Workspace\\Haven"));
        assert!(objects.contains(&"/opt/tools"));
        assert!(objects.contains(&"tidy"));
    }

    #[test]
    fn test_infer_rule_name() {
        let db = create_db();
        let msgs = vec![
            make_message("my name is Alice Johnson."),
            make_message("I am Bob"),
            make_message("call me Charlie."),
        ];
        let facts = db.infer_facts_from_messages(&msgs);
        assert_eq!(facts.len(), 3);
        for f in &facts {
            assert_eq!(f.predicate, "name");
            assert_eq!(f.confidence, 0.85);
            assert_eq!(f.tags, vec!["identity"]);
        }
    }

    #[test]
    fn test_infer_rule_name_skips_action_words() {
        let db = create_db();
        let msgs = vec![
            make_message("I am looking for a new laptop."),
            make_message("I am trying to fix the build."),
            make_message("I am working on a feature."),
        ];
        let facts = db.infer_facts_from_messages(&msgs);
        for f in &facts {
            assert_ne!(f.predicate, "name");
        }
    }

    #[test]
    fn test_infer_rule_uses() {
        let db = create_db();
        let msgs = vec![make_message("I use VSCode for development.")];
        let facts = db.infer_facts_from_messages(&msgs);
        assert_eq!(facts.len(), 1);
        assert_eq!(facts[0].predicate, "uses");
        assert_eq!(facts[0].object, "VSCode for development");
        assert_eq!(facts[0].confidence, 0.7);
        assert_eq!(facts[0].tags, vec!["preference"]);
    }

    #[test]
    fn test_infer_rule_works_at() {
        let db = create_db();
        let msgs = vec![
            make_message("I work at Acme Corp since last year."),
            make_message("I work for Tech Inc."),
        ];
        let facts = db.infer_facts_from_messages(&msgs);
        assert_eq!(facts.len(), 2);
        for f in &facts {
            assert_eq!(f.predicate, "works_at");
            assert_eq!(f.confidence, 0.75);
            assert_eq!(f.tags, vec!["identity"]);
        }
    }

    #[test]
    fn test_infer_rule_correction_lowers_confidence() {
        let db = create_db();
        db.insert_fact("user", "likes", "Rust", "inferred", 0.9, &["preference"])
            .unwrap();
        db.insert_fact("user", "likes", "Python", "inferred", 0.8, &["preference"])
            .unwrap();

        let msgs = vec![make_message("actually I prefer Go now.")];
        let facts = db.infer_facts_from_messages(&msgs);
        assert!(!facts.is_empty());

        let updated = db.get_facts("user").unwrap();
        for f in &updated {
            if f.predicate == "likes" && f.source == "inferred" {
                assert!(f.confidence < 0.9, "confidence should be lowered");
            }
        }
    }

    #[test]
    fn test_infer_multiple_rules_from_message() {
        let db = create_db();
        let msgs = vec![make_message(
            "I like Rust. I use VSCode. I work at Acme. My name is Dave.",
        )];
        let facts = db.infer_facts_from_messages(&msgs);
        let predicates: Vec<&str> = facts.iter().map(|f| f.predicate.as_str()).collect();
        assert!(predicates.contains(&"likes"));
        assert!(predicates.contains(&"uses"));
        assert!(predicates.contains(&"works_at"));
        assert!(predicates.contains(&"name"));
    }

    #[test]
    fn test_fact_cache_invalidation_on_insert() {
        let db = create_db();
        db.insert_fact("cache-test", "likes", "Rust", "user", 0.9, &[])
            .unwrap();
        let cached = db.get_facts("cache-test").unwrap();
        assert_eq!(cached.len(), 1);

        db.insert_fact("cache-test", "dislikes", "Java", "user", 0.5, &[])
            .unwrap();
        let fresh = db.get_facts("cache-test").unwrap();
        assert_eq!(fresh.len(), 2);
    }

    #[test]
    fn test_upsert_fact_inserts_new() {
        let db = create_db();
        let outcome = db
            .upsert_fact(
                "user",
                "likes",
                "Rust",
                "inferred",
                0.9,
                &["preference"],
                None,
            )
            .unwrap();
        assert_eq!(outcome, UpsertOutcome::Inserted);
        let facts = db.get_facts("user").unwrap();
        assert_eq!(facts.len(), 1);
        assert_eq!(facts[0].mention_count, 0);
        assert!(facts[0].last_seen_at.is_some());
    }

    #[test]
    fn test_upsert_polarity_conflict_demotes_opposite_inferred() {
        let db = create_db();
        db.upsert_fact("user", "likes", "Rust", "inferred", 0.9, &[], None)
            .unwrap();
        // New "dislikes Rust" observation halves the inferred likes fact.
        db.upsert_fact("user", "dislikes", "Rust", "inferred", 0.8, &[], None)
            .unwrap();
        let facts = db.get_facts("user").unwrap();
        let likes = facts.iter().find(|f| f.predicate == "likes").unwrap();
        assert!(
            (likes.confidence - 0.45).abs() < 1e-9,
            "likes confidence should be halved, got {}",
            likes.confidence
        );
        let dislikes = facts.iter().find(|f| f.predicate == "dislikes").unwrap();
        assert!((dislikes.confidence - 0.8).abs() < 1e-9);
    }

    #[test]
    fn test_upsert_polarity_conflict_user_wins_over_inferred() {
        let db = create_db();
        db.upsert_fact("user", "likes", "Rust", "inferred", 0.9, &[], None)
            .unwrap();
        // A user-stated dislike demotes even a high-confidence inferred like.
        db.upsert_fact("user", "dislikes", "Rust", "user", 1.0, &[], None)
            .unwrap();
        let likes = db
            .get_facts("user")
            .unwrap()
            .into_iter()
            .find(|f| f.predicate == "likes")
            .unwrap();
        assert!(
            (likes.confidence - 0.45).abs() < 1e-9,
            "user-stated dislike must demote inferred like"
        );
    }

    #[test]
    fn test_upsert_polarity_conflict_inferred_never_demotes_user() {
        let db = create_db();
        db.upsert_fact("user", "likes", "Rust", "user", 1.0, &[], None)
            .unwrap();
        // An inferred dislike must not touch a user-stated like.
        db.upsert_fact("user", "dislikes", "Rust", "inferred", 0.8, &[], None)
            .unwrap();
        let likes = db
            .get_facts("user")
            .unwrap()
            .into_iter()
            .find(|f| f.predicate == "likes")
            .unwrap();
        assert!(
            (likes.confidence - 1.0).abs() < 1e-9,
            "user-stated like must stay intact, got {}",
            likes.confidence
        );
    }

    #[test]
    fn test_upsert_fact_reinforces_existing() {
        let db = create_db();
        db.upsert_fact(
            "user",
            "likes",
            "Rust",
            "inferred",
            0.7,
            &["preference"],
            None,
        )
        .unwrap();
        db.upsert_fact(
            "user",
            "likes",
            "Rust",
            "inferred",
            0.8,
            &["preference"],
            None,
        )
        .unwrap();
        let outcome = db
            .upsert_fact(
                "user",
                "likes",
                "Rust",
                "inferred",
                0.9,
                &["preference"],
                None,
            )
            .unwrap();
        assert_eq!(outcome, UpsertOutcome::Reinforced);
        let facts = db.get_facts("user").unwrap();
        assert_eq!(facts.len(), 1);
        assert_eq!(facts[0].mention_count, 2);
        assert!(
            facts[0].confidence >= 0.7,
            "repeated confirmation should boost confidence, got {}",
            facts[0].confidence
        );
    }

    #[test]
    fn test_upsert_fact_single_valued_correction() {
        let db = create_db();
        db.upsert_fact(
            "user",
            "project_path",
            "/old/project",
            "inferred",
            0.7,
            &["workspace"],
            None,
        )
        .unwrap();
        db.upsert_fact(
            "user",
            "project_path",
            "/old/project",
            "inferred",
            0.8,
            &["workspace"],
            None,
        )
        .unwrap();
        let outcome = db
            .upsert_fact(
                "user",
                "project_path",
                "/new/project",
                "inferred",
                0.9,
                &["workspace"],
                None,
            )
            .unwrap();
        assert_eq!(outcome, UpsertOutcome::Corrected);
        let facts = db.get_facts("user").unwrap();
        let old = facts.iter().find(|f| f.object == "/old/project").unwrap();
        assert!(
            old.confidence <= 0.4,
            "superseded value should be demoted, got {}",
            old.confidence
        );
        let new = facts.iter().find(|f| f.object == "/new/project").unwrap();
        assert_eq!(new.confidence, 0.9);
    }

    #[test]
    fn test_upsert_fact_multi_valued_keeps_both() {
        let db = create_db();
        db.upsert_fact(
            "user",
            "likes",
            "Rust",
            "inferred",
            0.9,
            &["preference"],
            None,
        )
        .unwrap();
        db.upsert_fact(
            "user",
            "likes",
            "Go",
            "inferred",
            0.8,
            &["preference"],
            None,
        )
        .unwrap();
        let facts = db.get_facts("user").unwrap();
        assert_eq!(facts.len(), 2, "multi-valued predicates must coexist");
        assert!(facts.iter().all(|f| f.confidence >= 0.8));
    }

    #[test]
    fn test_upsert_fact_skips_inferred_when_user_value_exists() {
        let db = create_db();
        // A user-stated value is authoritative...
        db.upsert_fact(
            "user",
            "project_path",
            "/authoritative/project",
            "user",
            1.0,
            &["workspace"],
            None,
        )
        .unwrap();
        // ...so a later inferred value for the same single-valued predicate
        // must be dropped instead of stored alongside it.
        let outcome = db
            .upsert_fact(
                "user",
                "project_path",
                "/guessed/project",
                "inferred",
                0.9,
                &["workspace"],
                None,
            )
            .unwrap();
        assert_eq!(outcome, UpsertOutcome::Skipped);
        let facts = db.get_facts("user").unwrap();
        assert_eq!(facts.len(), 1, "inferred value must not be stored");
        assert_eq!(facts[0].object, "/authoritative/project");
    }

    #[test]
    fn test_upsert_fact_stores_and_updates_source_ref() {
        let db = create_db();
        let first = FactSourceRef {
            message_id: "m1".into(),
            snippet: "I like Rust".into(),
        };
        db.upsert_fact(
            "user",
            "likes",
            "Rust",
            "inferred",
            0.9,
            &["preference"],
            Some(&first),
        )
        .unwrap();
        let facts = db.get_facts("user").unwrap();
        let stored = facts[0].source_ref.as_ref().expect("source_ref stored");
        assert_eq!(stored.message_id, "m1");
        assert_eq!(stored.snippet, "I like Rust");

        // Reinforcement replaces the reference with the latest supporting
        // message.
        let second = FactSourceRef {
            message_id: "m2".into(),
            snippet: "still like Rust".into(),
        };
        db.upsert_fact(
            "user",
            "likes",
            "Rust",
            "inferred",
            0.9,
            &["preference"],
            Some(&second),
        )
        .unwrap();
        let facts = db.get_facts("user").unwrap();
        assert_eq!(facts.len(), 1);
        let updated = facts[0].source_ref.as_ref().unwrap();
        assert_eq!(updated.message_id, "m2");
        assert_eq!(facts[0].mention_count, 1);
    }

    #[test]
    fn test_upsert_fact_reinforcement_merges_tags() {
        let db = create_db();
        db.upsert_fact(
            "user",
            "likes",
            "Rust",
            "inferred",
            0.9,
            &["preference"],
            None,
        )
        .unwrap();
        db.upsert_fact(
            "user",
            "likes",
            "Rust",
            "inferred",
            0.9,
            &["preference", "workspace"],
            None,
        )
        .unwrap();
        let facts = db.get_facts("user").unwrap();
        assert_eq!(facts.len(), 1);
        assert!(
            facts[0].tags.contains(&"preference".to_string()),
            "existing tag must survive reinforcement"
        );
        assert!(
            facts[0].tags.contains(&"workspace".to_string()),
            "new tag must be merged on reinforcement"
        );
    }

    #[test]
    fn test_rule_inference_carries_source_ref() {
        let db = create_db();
        let msg = make_message("I work at Acme Corp since last year.");
        let inferred = db.infer_facts_from_messages(std::slice::from_ref(&msg));
        assert_eq!(inferred.len(), 1);
        let src = inferred[0]
            .source_ref
            .as_ref()
            .expect("rule facts carry a source reference");
        assert_eq!(src.message_id, msg.id);
        assert!(src.snippet.contains("Acme"));
    }

    #[test]
    fn test_effective_confidence_decays_volatile_but_not_identity() {
        let db = create_db();
        db.insert_fact("user", "name", "Alice", "user", 1.0, &["identity"])
            .unwrap();
        db.insert_fact(
            "user",
            "project_path",
            "/home/alice/proj",
            "inferred",
            0.9,
            &["workspace"],
        )
        .unwrap();

        // Age both facts by ~2 years.
        let old = "2024-01-01T00:00:00Z";
        let conn = db.conn();
        conn.execute(
            "UPDATE facts SET created_at = ?1, last_seen_at = ?1",
            rusqlite::params![old],
        )
        .unwrap();
        drop(conn);

        let facts = db.list_facts().unwrap();
        let name = facts.iter().find(|f| f.predicate == "name").unwrap();
        let path = facts
            .iter()
            .find(|f| f.predicate == "project_path")
            .unwrap();
        assert_eq!(
            fact_effective_confidence(name),
            1.0,
            "identity facts must not decay"
        );
        assert!(
            fact_effective_confidence(path) < 0.1,
            "volatile facts should decay hard after ~2 years, got {}",
            fact_effective_confidence(path)
        );
    }

    #[test]
    fn test_effective_confidence_user_sourced_never_decays() {
        let db = create_db();
        // A volatile predicate, but explicitly user-stated: must not decay.
        db.insert_fact(
            "user",
            "project_path",
            "/stable/project",
            "user",
            1.0,
            &["workspace"],
        )
        .unwrap();
        let conn = db.conn();
        conn.execute(
            "UPDATE facts SET created_at = '2024-01-01T00:00:00Z', last_seen_at = '2024-01-01T00:00:00Z'",
            [],
        )
        .unwrap();
        drop(conn);

        let facts = db.list_facts().unwrap();
        let stable = facts
            .iter()
            .find(|f| f.predicate == "project_path")
            .unwrap();
        assert_eq!(
            fact_effective_confidence(stable),
            1.0,
            "user-stated facts must not decay, even volatile predicates"
        );
    }

    #[test]
    fn test_flush_uses_effective_confidence() {
        let db = create_db();
        db.insert_fact("user", "likes", "Rust", "inferred", 0.5, &["preference"])
            .unwrap();
        db.insert_fact(
            "user",
            "project_path",
            "/gone/project",
            "inferred",
            0.9,
            &["workspace"],
        )
        .unwrap();
        let conn = db.conn();
        conn.execute(
            "UPDATE facts SET created_at = '2024-01-01T00:00:00Z', last_seen_at = '2024-01-01T00:00:00Z'
             WHERE predicate = 'project_path'",
            [],
        )
        .unwrap();
        drop(conn);

        // The old volatile fact decays far below the bar despite raw 0.9;
        // the fresh preference stays.
        let deleted = db.flush_low_confidence(0.45).unwrap();
        assert_eq!(deleted, 1);
        let remaining = db.list_facts().unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].object, "Rust");
    }

    #[test]
    fn test_search_facts_fts_multi_term_and() {
        let db = create_db();
        db.insert_fact("user", "uses", "VSCode", "user", 0.9, &["preference"])
            .unwrap();
        db.insert_fact("user", "uses", "IntelliJ", "user", 0.7, &["preference"])
            .unwrap();
        db.insert_fact("user", "likes", "Coffee", "user", 0.8, &["preference"])
            .unwrap();

        // Both terms must appear in a fact for an AND query.
        let results = db.search_facts("uses VSCode").unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].object, "VSCode");
        let results = db.search_facts("VSCode Coffee").unwrap();
        assert!(results.is_empty());
    }
}
