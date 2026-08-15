use crate::db::Database;
use chrono::{DateTime, Utc};
use std::collections::{HashMap, HashSet};

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
    /// 0..1 rating of how long this fact stays useful. Scales the effective
    /// confidence used for prompt ranking and pruning, so transient facts
    /// (low durability) die out fast while stable ones keep full weight.
    /// User-stated facts and identity predicates never decay regardless.
    #[serde(default = "default_durability")]
    pub durability: f64,
}

fn default_durability() -> f64 {
    1.0
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

/// Canonical SELECT column list for the facts table, in the exact positional
/// order `fact_from_row` maps (index 0..11). Single source of truth: every
/// facts SELECT is built from this const so a column add/remove cannot drift
/// a query away from the row mapper (mirrors `EMBED_COLS` in embeddings.rs).
const FACT_COLS: &str = "id, subject, predicate, object, source, confidence, tags, created_at, mention_count, last_seen_at, source_ref, durability";

/// Aliased variant for queries that prefix columns with a table alias
/// (FTS join).
const FACT_COLS_ALIASED: &str = "f.id, f.subject, f.predicate, f.object, f.source, f.confidence, f.tags, f.created_at, f.mention_count, f.last_seen_at, f.source_ref, f.durability";

/// Map a rusqlite Row (with the standard 12-column SELECT order) to a Fact.
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
        durability: row.get(11)?,
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

/// Canonical form for a fact predicate: trimmed + lowercase + alias mapping.
/// Every write path (inference persistence, `set_user_fact`, the facts tool)
/// normalizes so the same concept arriving under different spellings merges
/// into ONE row instead of fanning out — `Workspace` and `workspace_path`
/// and `project_location` all become `project_path`, keeping single-valued
/// constraints (and decay rules) effective across sources.
pub fn normalize_predicate(predicate: &str) -> String {
    let p = predicate.trim().to_ascii_lowercase();
    match p.as_str() {
        "workspace" | "workspace_path" | "project_location" | "working_directory"
        | "working_dir" => "project_path".into(),
        "employer" | "company_name" => "works_at".into(),
        "favorite_language" | "preferred_language" => "language".into(),
        "preferred_verbosity" | "verbosity_level" => "verbosity".into(),
        "preferred_shell" | "shell_choice" => "shell".into(),
        "os_name" | "operating_system" => "os".into(),
        _ => p,
    }
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
                | "action"
                | "role"
                | "shell"
                | "os"
                | "location"
                | "address"
                | "language"
                | "verbosity"
        )
}

/// Parse a fact timestamp (always RFC3339 — the repository writes
/// `Utc::now().to_rfc3339()`). Unparseable timestamps (corrupt rows) fall
/// back to "now" so recency math cannot panic or poison the sort.
fn parse_fact_time(s: &str) -> DateTime<Utc> {
    if let Ok(dt) = DateTime::parse_from_rfc3339(s) {
        return dt.with_timezone(&Utc);
    }
    Utc::now()
}

fn fact_age_days(fact: &Fact) -> f64 {
    let ts = fact.last_seen_at.as_deref().unwrap_or(&fact.created_at);
    (Utc::now() - parse_fact_time(ts)).num_days() as f64
}

/// Effective confidence after recency decay and durability. Identity facts
/// never decay, and neither do explicitly user-stated facts (`source="user"`,
/// e.g. added via the settings UI — the user can remove those, time should
/// not). Volatile facts decay with a 90-day half-life; everything else
/// (preferences etc.) with a 365-day half-life. `durability` scales the
/// result (1.0 = full weight, 0.3 = a third), so low-durability facts sink
/// below the flush threshold and get pruned even when freshly extracted,
/// while durable facts keep their weight. Old inferred facts that stop being
/// re-confirmed decay the same way and get pruned.
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

/// Batch existence result for fact inference: the exact
/// (subject, predicate, object) triples and the (subject, predicate) pairs
/// already stored for a batch of subjects.
pub type FactPresence = (HashSet<(String, String, String)>, HashSet<(String, String)>);

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
        self.insert_fact_with_source_ref(
            subject, predicate, object, source, confidence, tags, None, 1.0,
        )
    }

    /// Insert a fact with an optional reference to the message it came from
    /// and an explicit durability rating (0..1). The predicate is normalized
    /// to its canonical form ([`Self::normalize_predicate`] via
    /// [`normalize_predicate`]) so the same concept from any source merges
    /// into one row.
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
        durability: f64,
    ) -> anyhow::Result<Fact> {
        let predicate = normalize_predicate(predicate);
        let id = haven_common::types::new_id("fact");
        let now = Utc::now().to_rfc3339();
        let tags_json = serialize_tags(tags);
        let source_ref_json = serialize_source_ref(source_ref);
        let conn = self.conn();
        conn.execute(
            "INSERT INTO facts (id, subject, predicate, object, source, confidence, created_at, tags, mention_count, last_seen_at, source_ref, durability)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 0, ?9, ?10, ?11)",
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
                source_ref_json,
                durability
            ],
        )?;
        self.cache_invalidate_facts(subject);
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

    /// Store a fact the user explicitly stated (e.g. via the `remember_fact`
    /// tool or the settings UI). User-stated facts are authoritative:
    ///
    /// - Same (subject, predicate, object) triple already present as a
    ///   user fact → reinforcement (bump `mention_count`, refresh
    ///   `last_seen_at`, raise confidence to 1.0).
    /// - Same triple present as an inferred fact → the user just confirmed
    ///   it, so the row is upgraded to `source="user"`.
    /// - The predicate is single-valued (name, language, verbosity, ...) →
    ///   every other value for that predicate (user or inferred) is removed
    ///   first, so the new statement strictly replaces the old one.
    /// - Multi-valued predicates (likes, uses, ...) → plain insert; the
    ///   new value coexists with the existing ones.
    ///
    /// The connection guard is scoped per statement: `insert_fact` (and any
    /// other `&self` method) re-locks `self.conn`, and `std::sync::Mutex` is
    /// not reentrant — holding the guard across the call would deadlock.
    pub fn set_user_fact(
        &self,
        subject: &str,
        predicate: &str,
        object: &str,
        tags: &[&str],
    ) -> anyhow::Result<Fact> {
        let predicate = normalize_predicate(predicate);
        let triple_exists: Option<Fact> = {
            let conn = self.conn();
            conn.query_row(
                &format!("SELECT {FACT_COLS} FROM facts WHERE subject = ?1 AND predicate = ?2 AND object = ?3"),
                rusqlite::params![subject, predicate, object],
                fact_from_row,
            )
            .ok()
        };
        if let Some(existing) = triple_exists {
            if existing.source == "user" {
                // Reinforcement: re-confirmed by the user, confidence maxed.
                {
                    let conn = self.conn();
                    conn.execute(
                        "UPDATE facts
                         SET mention_count = mention_count + 1, last_seen_at = ?1, confidence = 1.0,
                             durability = 1.0
                         WHERE id = ?2",
                        rusqlite::params![Utc::now().to_rfc3339(), existing.id],
                    )?;
                }
                self.cache_invalidate_facts(subject);
                self.cache_invalidate_embeddings(crate::embeddings::entity_kind::FACT);
                let mut fact = existing;
                fact.confidence = 1.0;
                fact.durability = 1.0;
                fact.mention_count += 1;
                fact.last_seen_at = Some(Utc::now().to_rfc3339());
                return Ok(fact);
            }
            // Upgrade an inferred row to user-stated.
            {
                let conn = self.conn();
                conn.execute(
                    "UPDATE facts SET source = 'user', confidence = 1.0, last_seen_at = ?1, durability = 1.0 WHERE id = ?2",
                    rusqlite::params![Utc::now().to_rfc3339(), existing.id],
                )?;
            }
            self.cache_invalidate_facts(subject);
            self.cache_invalidate_embeddings(crate::embeddings::entity_kind::FACT);
            let mut fact = existing;
            fact.source = "user".into();
            fact.confidence = 1.0;
            fact.durability = 1.0;
            fact.last_seen_at = Some(Utc::now().to_rfc3339());
            return Ok(fact);
        }
        if is_single_valued_predicate(&predicate) {
            let conn = self.conn();
            conn.execute(
                "DELETE FROM facts WHERE subject = ?1 AND predicate = ?2",
                rusqlite::params![subject, predicate],
            )?;
            self.cache_invalidate_embeddings(crate::embeddings::entity_kind::FACT);
        }
        self.insert_fact(subject, &predicate, object, "user", 1.0, tags)
    }

    /// Delete facts by (subject, predicate[, object]) — used by the
    /// `forget_fact` tool and the settings UI. When `object` is `None`,
    /// every fact with that subject+predicate is removed (both sources).
    /// Returns the number of deleted rows.
    pub fn delete_facts_by_triple(
        &self,
        subject: &str,
        predicate: &str,
        object: Option<&str>,
    ) -> anyhow::Result<u64> {
        let predicate = normalize_predicate(predicate);
        let conn = self.conn();
        let deleted = match object {
            Some(obj) => conn.execute(
                "DELETE FROM facts WHERE subject = ?1 AND predicate = ?2 AND object = ?3",
                rusqlite::params![subject, predicate, obj],
            )?,
            None => conn.execute(
                "DELETE FROM facts WHERE subject = ?1 AND predicate = ?2",
                rusqlite::params![subject, predicate],
            )?,
        };
        self.cache_invalidate_facts(subject);
        self.cache_invalidate_embeddings(crate::embeddings::entity_kind::FACT);
        Ok(deleted as u64)
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
                &format!("SELECT {FACT_COLS} FROM facts WHERE subject = ?1 AND predicate = ?2 AND object = ?3"),
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
    /// reinforcement it replaces the stored reference when provided. The
    /// durability variant additionally records the incoming 0..1 durability
    /// rating (reinforcement keeps the higher of the two).
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
        self.upsert_fact_with_durability(
            subject, predicate, object, source, confidence, tags, source_ref, 1.0,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn upsert_fact_with_durability(
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
        // §P2: polarity conflict — "likes X" and "dislikes X" contradict each
        // other; the newest observation demotes the opposite-polarity fact so
        // the prompt never shows both. User-stated facts always win: an
        // inferred fact never demotes a user-stated opposite.
        let opposite = match predicate.as_str() {
            "likes" => Some("dislikes"),
            "dislikes" => Some("likes"),
            _ => None,
        };
        {
            let conn = self.conn();
            let existing: Option<Fact> = conn
                .query_row(
                    &format!("SELECT {FACT_COLS} FROM facts WHERE subject = ?1 AND predicate = ?2 AND object = ?3"),
                    rusqlite::params![subject, predicate, object],
                    fact_from_row,
                )
                .ok();
            if let Some(existing) = existing {
                // Reinforcement: repeated confirmation keeps a fact alive and
                // nudges its confidence up (capped at 1.0, never below incoming).
                // Durability merges upward: a re-confirmed durable fact stays
                // durable, and a re-extraction that raises durability keeps it.
                let boosted = (existing.confidence * 1.05).min(1.0).max(confidence);
                let merged_durability = existing.durability.max(durability).clamp(0.0, 1.0);
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
                         source_ref = ?3, tags = ?4, durability = ?5
                     WHERE id = ?6",
                    rusqlite::params![
                        now,
                        boosted,
                        serialize_source_ref(merged_ref),
                        serialize_tags(&tag_refs),
                        merged_durability,
                        existing.id
                    ],
                )?;
                self.cache_invalidate_facts(subject);
                self.cache_invalidate_embeddings(crate::embeddings::entity_kind::FACT);
                return Ok(UpsertOutcome::Reinforced);
            }

            if is_single_valued_predicate(&predicate) {
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
        // The demotion UPDATEs above fire the facts_embed_upd trigger (the
        // only other non-insert path, reinforcement, returned early above).
        // Invalidate the embeddings list cache accordingly.
        if corrected || opposite.is_some() {
            self.cache_invalidate_embeddings(crate::embeddings::entity_kind::FACT);
        }
        let _ = self.insert_fact_with_source_ref(
            subject, &predicate, object, source, confidence, tags, source_ref, durability,
        )?;
        Ok(if corrected {
            UpsertOutcome::Corrected
        } else {
            UpsertOutcome::Inserted
        })
    }

    /// Fetch a single fact by id. Used by the prompt builder to resolve
    /// vector-recall hits (`search_embeddings` returns entity ids) back into
    /// full facts for ranking and rendering.
    pub fn get_fact_by_id(&self, id: &str) -> anyhow::Result<Option<Fact>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!("SELECT {FACT_COLS} FROM facts WHERE id = ?1"))?;
        let mut rows = stmt.query(rusqlite::params![id])?;
        match rows.next()? {
            Some(row) => Ok(Some(fact_from_row(row)?)),
            None => Ok(None),
        }
    }

    /// Fetch multiple facts by id in a single query (input order preserved).
    /// Used by the prompt builder to resolve a batch of vector-recall hits in
    /// one round-trip instead of one query per id.
    pub fn get_facts_by_ids(&self, ids: &[String]) -> anyhow::Result<Vec<Fact>> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let conn = self.conn();
        let placeholders: Vec<String> = (1..=ids.len()).map(|i| format!("?{i}")).collect();
        let sql = format!(
            "SELECT {FACT_COLS} FROM facts WHERE id IN ({})",
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
        let key = format!("_facts_{}", subject);
        let cache_gen = self.cache_generation(&key);
        let conn = self.conn();
        let mut stmt =
            conn.prepare(&format!("SELECT {FACT_COLS} FROM facts WHERE subject = ?1"))?;
        let rows = stmt.query_map(rusqlite::params![subject], fact_from_row)?;
        let mut facts = Vec::new();
        for row in rows {
            facts.push(row?);
        }
        sort_facts_effective(&mut facts);
        self.cache_put_facts(subject, facts.clone(), 60, cache_gen);
        Ok(facts)
    }

    /// Whether an exact (subject, predicate, object) triple is already
    /// stored. Used to distinguish a re-confirmation of an existing fact
    /// (which must reinforce it regardless of the incoming confidence) from a
    /// brand-new fact (which is subject to the persist confidence floor).
    pub fn fact_triple_exists(&self, subject: &str, predicate: &str, object: &str) -> bool {
        let conn = self.conn();
        conn.query_row(
            "SELECT 1 FROM facts WHERE subject = ?1 AND predicate = ?2 AND object = ?3 LIMIT 1",
            rusqlite::params![subject, predicate, object],
            |_| Ok(()),
        )
        .is_ok()
    }

    /// Whether any fact is already stored for a (subject, predicate) pair,
    /// regardless of the object value. Used to recognize a single-valued
    /// UPDATE: the user changed the value, so the new triple does not match
    /// [`Self::fact_triple_exists`], but a stored value for the predicate
    /// means the extraction is a correction that must replace it rather than
    /// being subject to the new-fact confidence floor.
    pub fn fact_predicate_exists(&self, subject: &str, predicate: &str) -> bool {
        let conn = self.conn();
        conn.query_row(
            "SELECT 1 FROM facts WHERE subject = ?1 AND predicate = ?2 LIMIT 1",
            rusqlite::params![subject, predicate],
            |_| Ok(()),
        )
        .is_ok()
    }

    /// Batch existence check for fact inference: one query returns (a) the
    /// exact (subject, predicate, object) triples already stored for the
    /// given subjects and (b) the (subject, predicate) pairs present — the
    /// inputs of [`Self::fact_triple_exists`] / [`Self::fact_predicate_exists`]
    /// for an entire batch. Lets `persist_fact_batch` resolve new-fact vs
    /// single-valued-update in one round trip instead of two per fact.
    pub fn facts_exist_batch(&self, subjects: &[&str]) -> anyhow::Result<FactPresence> {
        if subjects.is_empty() {
            return Ok((HashSet::new(), HashSet::new()));
        }
        let placeholders = vec!["?"; subjects.len()].join(",");
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!(
            "SELECT subject, predicate, object FROM facts WHERE subject IN ({placeholders})"
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

    /// All facts in effective-confidence order. Cached (generation-guarded,
    /// same policy as `get_facts`) because it is a per-extraction hot path
    /// (`load_known_facts`) — an uncached full-table scan + JSON decode per
    /// call would be wasteful. Invalidated together with the subject caches
    /// on any fact mutation.
    pub fn list_facts(&self) -> anyhow::Result<Vec<Fact>> {
        if let Some(cached) = self.cache_get_facts_all() {
            return Ok(cached);
        }
        let cache_gen = self.cache_generation("_facts_all");
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!("SELECT {FACT_COLS} FROM facts"))?;
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
        let mut stmt = conn.prepare(&format!("SELECT {FACT_COLS} FROM facts WHERE source = ?1"))?;
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
        let fts_sql = format!(
            "SELECT {FACT_COLS_ALIASED}
                               FROM facts f
                               JOIN facts_fts ON f.rowid = facts_fts.rowid
                               WHERE facts_fts MATCH ?1
                               ORDER BY bm25(facts_fts)"
        );
        if let Ok(mut stmt) = conn.prepare(&fts_sql)
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
        let mut stmt = conn.prepare(&format!(
            "SELECT {FACT_COLS} FROM facts
             WHERE subject LIKE ?1 OR predicate LIKE ?1 OR object LIKE ?1 OR tags LIKE ?1"
        ))?;
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
        let mut stmt = conn.prepare(&format!(
            "SELECT {FACT_COLS} FROM facts
             WHERE EXISTS (SELECT 1 FROM json_each(facts.tags) AS te WHERE te.value = ?1)"
        ))?;
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
        self.cache_invalidate_embeddings(crate::embeddings::entity_kind::FACT);
        Ok(())
    }

    pub fn dedup_facts(&self) -> anyhow::Result<u64> {
        // Group by (subject, predicate, object) regardless of tags: the same
        // triple is the same fact even when older rows carry a different (or
        // empty) tag set. The previous tag-sensitive grouping let repeated
        // re-extraction pile up duplicates — e.g. 379 rows of `name=Xtopia`.
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!("SELECT {FACT_COLS} FROM facts"))?;
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
        self.cache_invalidate_embeddings(crate::embeddings::entity_kind::FACT);
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
    ///
    /// Facts observed within the last day are exempt: without the grace
    /// period, a brand-new fact that passes the persist floor but carries a
    /// low durability rating (effective confidence = confidence × durability
    /// < threshold) would be written and pruned in the same maintenance pass,
    /// voiding the persist floor and wasting the extraction. The grace window
    /// gives every persisted fact at least one full recall cycle; decay and
    /// durability still prune it from the second day on.
    pub fn flush_low_confidence(&self, threshold: f64) -> anyhow::Result<u64> {
        let facts = self.list_facts()?;
        let mut stale_ids: Vec<String> = Vec::new();
        for fact in &facts {
            if fact_effective_confidence(fact) < threshold && fact_age_days(fact) >= 1.0 {
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
        self.cache_invalidate_embeddings(crate::embeddings::entity_kind::FACT);
        Ok(count)
    }
}

#[cfg(test)]
mod tests {
    use super::{FactSourceRef, UpsertOutcome, fact_effective_confidence};
    use crate::Database;

    fn create_db() -> Database {
        Database::open_in_memory().unwrap()
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

        // trigram tokenizer is substring-based: "likes" matches both the
        // likes predicate and the "likes" inside "dislikes" (the price of CJK
        // substring support; BM25 ranking decides the order between them).
        let results = db.search_facts("likes").unwrap();
        assert_eq!(results.len(), 2);
        let preds: Vec<&str> = results.iter().map(|f| f.predicate.as_str()).collect();
        assert!(preds.contains(&"likes") && preds.contains(&"dislikes"));
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
    fn test_set_user_fact_inserts_user_sourced() {
        let db = create_db();
        db.set_user_fact("user", "email", "alice@example.com", &["identity"])
            .unwrap();
        let facts = db.get_facts("user").unwrap();
        assert_eq!(facts.len(), 1);
        assert_eq!(facts[0].predicate, "email");
        assert_eq!(facts[0].source, "user");
        assert_eq!(facts[0].confidence, 1.0);
    }

    #[test]
    fn test_set_user_fact_replaces_single_valued() {
        let db = create_db();
        // Inferred old value exists; the explicit statement must replace it.
        db.insert_fact(
            "user",
            "language",
            "Chinese",
            "inferred",
            0.75,
            &["preference"],
        )
        .unwrap();
        db.set_user_fact("user", "language", "English", &["preference"])
            .unwrap();
        let languages: Vec<_> = db
            .get_facts("user")
            .unwrap()
            .into_iter()
            .filter(|f| f.predicate == "language")
            .collect();
        assert_eq!(
            languages.len(),
            1,
            "single-valued must be replaced, not coexisting"
        );
        assert_eq!(languages[0].object, "English");
        assert_eq!(languages[0].source, "user");
    }

    #[test]
    fn test_set_user_fact_upgrades_inferred_triple() {
        let db = create_db();
        db.insert_fact("user", "uses", "VSCode", "inferred", 0.7, &["preference"])
            .unwrap();
        db.set_user_fact("user", "uses", "VSCode", &["preference"])
            .unwrap();
        let uses: Vec<_> = db
            .get_facts("user")
            .unwrap()
            .into_iter()
            .filter(|f| f.predicate == "uses")
            .collect();
        assert_eq!(uses.len(), 1);
        assert_eq!(
            uses[0].source, "user",
            "explicit confirmation must upgrade to user"
        );
        assert_eq!(uses[0].confidence, 1.0);
    }

    #[test]
    fn test_set_user_fact_multi_valued_coexists() {
        let db = create_db();
        db.set_user_fact("user", "likes", "Rust", &["preference"])
            .unwrap();
        db.set_user_fact("user", "likes", "Go", &["preference"])
            .unwrap();
        let likes: Vec<_> = db
            .get_facts("user")
            .unwrap()
            .into_iter()
            .filter(|f| f.predicate == "likes")
            .collect();
        assert_eq!(likes.len(), 2, "multi-valued predicates accumulate");
    }

    #[test]
    fn test_predicate_normalization_merges_aliases() {
        let db = create_db();
        // Alias spellings collapse onto the canonical predicate...
        db.set_user_fact("user", "Workspace", "D:/dev/app", &["workspace"])
            .unwrap();
        let facts = db.get_facts("user").unwrap();
        assert_eq!(facts.len(), 1);
        assert_eq!(facts[0].predicate, "project_path");
        // ...so the single-valued constraint replaces, not duplicates, and a
        // differently-spelled delete still removes the row.
        db.set_user_fact("user", "workspace_path", "D:/dev/other", &["workspace"])
            .unwrap();
        let facts = db.get_facts("user").unwrap();
        assert_eq!(facts.len(), 1);
        assert_eq!(facts[0].object, "D:/dev/other");
        let deleted = db
            .delete_facts_by_triple("user", "project_location", Some("D:/dev/other"))
            .unwrap();
        assert_eq!(deleted, 1);
        // upsert path normalizes too.
        db.upsert_fact("user", "Employer", "Acme", "inferred", 0.9, &[], None)
            .unwrap();
        let facts = db.get_facts("user").unwrap();
        assert_eq!(facts[0].predicate, "works_at");
    }

    #[test]
    fn test_delete_facts_by_triple_object_and_predicate() {
        let db = create_db();
        db.insert_fact("user", "uses", "VSCode", "user", 1.0, &["preference"])
            .unwrap();
        db.insert_fact("user", "uses", "IntelliJ", "inferred", 0.6, &["preference"])
            .unwrap();
        db.insert_fact("user", "likes", "Rust", "user", 1.0, &["preference"])
            .unwrap();

        // Delete a specific value.
        let n = db
            .delete_facts_by_triple("user", "uses", Some("VSCode"))
            .unwrap();
        assert_eq!(n, 1);
        let remaining: Vec<_> = db
            .get_facts("user")
            .unwrap()
            .into_iter()
            .filter(|f| f.predicate == "uses")
            .collect();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].object, "IntelliJ");

        // Delete the whole predicate (both sources).
        let n = db.delete_facts_by_triple("user", "uses", None).unwrap();
        assert_eq!(n, 1);
        let uses: Vec<_> = db
            .get_facts("user")
            .unwrap()
            .into_iter()
            .filter(|f| f.predicate == "uses")
            .collect();
        assert!(uses.is_empty());
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

    /// Age every fact past the 1-day flush grace period so decay/flush
    /// thresholds apply (fresh facts are exempt by design). The raw SQL write
    /// bypasses the repository methods (and fires the facts_embed_upd
    /// trigger), so both caches are invalidated here too — otherwise the next
    /// list_facts would keep serving the fresh rows.
    fn age_facts(db: &Database, days: i64) {
        let old = (chrono::Utc::now() - chrono::Duration::days(days)).to_rfc3339();
        let conn = db.conn();
        conn.execute(
            "UPDATE facts SET created_at = ?1, last_seen_at = ?1",
            rusqlite::params![old],
        )
        .unwrap();
        drop(conn);
        db.cache_invalidate_facts("user");
        db.cache_invalidate_embeddings(crate::embeddings::entity_kind::FACT);
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
        age_facts(&db, 2);

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
        age_facts(&db, 2);

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
        age_facts(&db, 2);

        let count = db.flush_low_confidence(1.0).unwrap();
        assert_eq!(count, 2);
        let remaining = db.list_facts().unwrap();
        assert!(remaining.is_empty());
    }

    #[test]
    fn test_flush_exempts_facts_within_grace_period() {
        let db = create_db();
        // A brand-new low-durability fact below the flush bar: the grace
        // period must keep it (a fresh extraction must never be written and
        // pruned in the same maintenance pass).
        db.upsert_fact_with_durability(
            "user",
            "likes",
            "Transient",
            "inferred",
            0.55,
            &["preference"],
            None,
            0.5,
        )
        .unwrap();
        assert_eq!(db.flush_low_confidence(0.3).unwrap(), 0);
        assert_eq!(db.list_facts().unwrap().len(), 1);
        // Once observed long ago, the same fact sinks and is pruned.
        age_facts(&db, 2);
        assert_eq!(db.flush_low_confidence(0.3).unwrap(), 1);
        assert!(db.list_facts().unwrap().is_empty());
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

    #[test]
    fn test_insert_fact_defaults_durability_one() {
        let db = create_db();
        let fact = db
            .insert_fact("user", "likes", "Rust", "inferred", 0.9, &["preference"])
            .unwrap();
        assert_eq!(fact.durability, 1.0);
        // Round-trips through the DB.
        let loaded = db.get_facts("user").unwrap();
        assert_eq!(loaded[0].durability, 1.0);
    }

    #[test]
    fn test_upsert_with_durability_stores_and_merges_max() {
        let db = create_db();
        // New fact with a low durability rating (transient observation).
        db.upsert_fact_with_durability(
            "user",
            "likes",
            "Rust",
            "inferred",
            0.9,
            &["preference"],
            None,
            0.3,
        )
        .unwrap();
        let facts = db.get_facts("user").unwrap();
        assert_eq!(facts[0].durability, 0.3);
        // Re-confirmation with a higher durability merges upward.
        db.upsert_fact_with_durability(
            "user",
            "likes",
            "Rust",
            "inferred",
            0.95,
            &["preference"],
            None,
            0.9,
        )
        .unwrap();
        let facts = db.get_facts("user").unwrap();
        assert_eq!(facts[0].durability, 0.9);
        assert_eq!(facts[0].mention_count, 1);
        // A lower durability never drags an existing durable fact down.
        db.upsert_fact_with_durability(
            "user",
            "likes",
            "Rust",
            "inferred",
            0.8,
            &["preference"],
            None,
            0.2,
        )
        .unwrap();
        let facts = db.get_facts("user").unwrap();
        assert_eq!(facts[0].durability, 0.9);
    }

    #[test]
    fn test_effective_confidence_scaled_by_durability() {
        let db = create_db();
        db.upsert_fact_with_durability(
            "user",
            "likes",
            "Durable",
            "inferred",
            0.9,
            &["preference"],
            None,
            1.0,
        )
        .unwrap();
        db.upsert_fact_with_durability(
            "user",
            "likes",
            "Transient",
            "inferred",
            0.9,
            &["preference"],
            None,
            0.2,
        )
        .unwrap();
        let facts = db.list_facts().unwrap();
        let durable = facts.iter().find(|f| f.object == "Durable").unwrap();
        let transient = facts.iter().find(|f| f.object == "Transient").unwrap();
        // Same raw confidence, same age — durability alone decides the weight.
        assert!(
            (fact_effective_confidence(durable) - 0.9).abs() < 1e-9,
            "durability 1.0 keeps full weight"
        );
        assert!(
            (fact_effective_confidence(transient) - 0.18).abs() < 1e-9,
            "durability 0.2 scales the effective confidence to 0.18"
        );
        // A low-durability fact sinks below the flush bar while its durable
        // twin survives. Both facts are aged past the grace period first so
        // decay-based pruning applies to them.
        age_facts(&db, 2);
        let deleted = db.flush_low_confidence(0.3).unwrap();
        assert_eq!(deleted, 1);
        let remaining = db.list_facts().unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].object, "Durable");
    }
}
