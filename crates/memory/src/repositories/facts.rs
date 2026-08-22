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

/// Canonical merge targets for maintenance LLM predicate rewrites (M6).
/// Shared by the gate and the merge system prompt so the two cannot drift.
pub const CANONICAL_MERGE_TARGETS: &[&str] = &[
    "name",
    "birthday",
    "email",
    "phone",
    "city",
    "country",
    "timezone",
    "works_at",
    "project_path",
    "language",
    "likes",
    "dislikes",
    "uses",
    "verbosity",
    "shell",
    "os",
    "location",
    "role",
    "address",
];

/// True when `predicate` (already normalized / lowercase) is an allowed
/// maintenance merge target (M6).
pub fn is_canonical_merge_target(predicate: &str) -> bool {
    CANONICAL_MERGE_TARGETS
        .iter()
        .any(|t| predicate.eq_ignore_ascii_case(t))
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
        | "working_dir" | "cwd" | "work_dir" => "project_path".into(),
        // P2-11: bare `company` (and common job/employer spellings) → works_at.
        "employer" | "company_name" | "company" | "workplace" | "job" | "employer_name" => {
            "works_at".into()
        }
        "favorite_language" | "preferred_language" | "lang" | "prog_language" => "language".into(),
        "preferred_verbosity" | "verbosity_level" => "verbosity".into(),
        "preferred_shell" | "shell_choice" => "shell".into(),
        "os_name" | "operating_system" | "platform" => "os".into(),
        "full_name" | "user_name" | "username" => "name".into(),
        "home_city" | "lives_in" => "city".into(),
        "home_country" | "lives_in_country" => "country".into(),
        "tz" | "time_zone" => "timezone".into(),
        "job_title" | "job_role" | "title" => "role".into(),
        _ => p,
    }
}

/// Predicates that change over time (paths, employers, tooling): these decay
/// fastest so stale values drop out of the prompt once they are no longer
/// confirmed. Entries are **canonical** names only — aliases are rewritten by
/// [`normalize_predicate`] before these checks run (P2-11).
pub fn is_volatile_predicate(predicate: &str) -> bool {
    matches!(
        predicate.to_ascii_lowercase().as_str(),
        "project_path" | "works_at" | "uses"
    )
}

/// Predicates that hold a single value per subject at any point in time. When
/// a new fact with the same predicate but a different object is extracted, the
/// old inferred values are demoted instead of coexisting (a new project path
/// supersedes the old one; a user can like both Rust and Go and use several
/// tools at once though). Canonical names only (P2-11).
pub fn is_single_valued_predicate(predicate: &str) -> bool {
    is_identity_predicate(predicate)
        || matches!(
            predicate.to_ascii_lowercase().as_str(),
            "project_path"
                | "works_at"
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

/// Free-text provenance / snippets: treat as sensitive when they look like
/// credential *objects* **or** contain credential *keywords* (e.g.
/// "password is …", "token=…") that `is_sensitive_object` alone would miss.
pub fn is_sensitive_text(text: &str) -> bool {
    if is_sensitive_object(text) {
        return true;
    }
    let t = text.to_ascii_lowercase();
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
    SENSITIVE_KEYWORDS.iter().any(|k| t.contains(k))
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

    /// Seed set for prompt recall: top-`limit` facts for a subject by raw
    /// `confidence` in SQL (then resorted by effective confidence in Rust).
    /// Avoids the full-subject pull that `get_facts` uses for tools/UI.
    /// Not cached — prompt builds are infrequent relative to tool list, and a
    /// separate limited cache would drift from the full subject cache.
    pub fn get_facts_limited(&self, subject: &str, limit: usize) -> anyhow::Result<Vec<Fact>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!(
            "SELECT {FACT_COLS} FROM facts WHERE subject = ?1
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

    /// Batch existence check for fact inference: one query returns (a) the
    /// exact (subject, predicate, object) triples already stored for the
    /// given subjects and (b) the (subject, predicate) pairs present.
    /// Lets `persist_fact_batch` resolve new-fact vs single-valued-update in
    /// one round trip instead of two per fact.
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
        Self::build_fts_query_joined(terms, " AND ")
    }

    /// Same quoting as [`Self::build_fts_query`], but OR-combined so a fact
    /// matching any one session keyword can surface in prompt recall.
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

    /// Escape `%`, `_`, and `\` so LIKE patterns match literally.
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

    /// Terms shorter than a trigram may miss FTS and need LIKE.
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

    /// Run FTS MATCH; `Ok(None)` means prepare/MATCH failed (caller may LIKE).
    /// `Ok(Some(rows))` is a successful query (possibly empty).
    fn search_facts_fts(
        &self,
        match_expr: &str,
        limit: Option<usize>,
    ) -> anyhow::Result<Option<Vec<Fact>>> {
        let conn = self.conn();
        let (fts_sql, bind_limit) = if let Some(lim) = limit {
            (
                format!(
                    "SELECT {FACT_COLS_ALIASED}
                     FROM facts f
                     JOIN facts_fts ON f.rowid = facts_fts.rowid
                     WHERE facts_fts MATCH ?1
                     ORDER BY bm25(facts_fts)
                     LIMIT ?2"
                ),
                Some(lim as i64),
            )
        } else {
            (
                format!(
                    "SELECT {FACT_COLS_ALIASED}
                     FROM facts f
                     JOIN facts_fts ON f.rowid = facts_fts.rowid
                     WHERE facts_fts MATCH ?1
                     ORDER BY bm25(facts_fts)"
                ),
                None,
            )
        };
        let Ok(mut stmt) = conn.prepare(&fts_sql) else {
            return Ok(None);
        };
        let rows = if let Some(lim) = bind_limit {
            stmt.query_map(rusqlite::params![match_expr, lim], fact_from_row)
        } else {
            stmt.query_map(rusqlite::params![match_expr], fact_from_row)
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

    /// OR of escaped LIKE patterns across subject/predicate/object/tags.
    fn search_facts_like_any(&self, terms: &[&str], limit: usize) -> anyhow::Result<Vec<Fact>> {
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
        let limit_param = terms.len() + 1;
        let sql = format!(
            "SELECT {FACT_COLS} FROM facts
             WHERE {}
             ORDER BY confidence DESC, COALESCE(last_seen_at, created_at) DESC
             LIMIT ?{limit_param}",
            clauses.join(" OR ")
        );
        let mut stmt = conn.prepare(&sql)?;
        let mut params: Vec<rusqlite::types::Value> = patterns
            .into_iter()
            .map(rusqlite::types::Value::Text)
            .collect();
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

    /// Full-text search across subject, predicate, object, and tags. Uses the
    /// FTS5 trigram index (BM25) when available. Empty FTS on a long query is
    /// treated as a true miss (P2-14); LIKE is reserved for short terms
    /// (&lt; 3 chars / CJK digrams) that trigram cannot index, or when FTS is
    /// unavailable entirely.
    pub fn search_facts(&self, query: &str) -> anyhow::Result<Vec<Fact>> {
        let terms: Vec<&str> = query.split_whitespace().collect();
        if terms.is_empty() {
            return Ok(Vec::new());
        }
        let match_expr = Self::build_fts_query(&terms);
        let short = Self::short_like_terms(&terms);
        match self.search_facts_fts(&match_expr, None)? {
            Some(facts) if short.is_empty() => {
                // Long-only query: empty FTS = true miss under trigram (P2-14).
                return Ok(facts);
            }
            Some(facts) => {
                // Digrams miss trigram MATCH — LIKE short terms and merge.
                let like = self.search_facts_like_any(&short, 50)?;
                return Ok(Self::merge_facts_limited(facts, like, 50));
            }
            None if short.is_empty() => {
                // FTS unavailable: whole-query LIKE (escaped).
            }
            None => {
                // Prefer short LIKE; only then long-term LIKE if still empty.
                let like_short = self.search_facts_like_any(&short, 50)?;
                if !like_short.is_empty() {
                    return Ok(like_short);
                }
            }
        }
        // FTS unavailable (or short LIKE empty with no FTS): escaped LIKE on
        // the full query string.
        let pattern = format!("%{}%", Self::escape_like_term(query));
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!(
            "SELECT {FACT_COLS} FROM facts
             WHERE subject LIKE ?1 ESCAPE '\\' OR predicate LIKE ?1 ESCAPE '\\'
                OR object LIKE ?1 ESCAPE '\\' OR tags LIKE ?1 ESCAPE '\\'"
        ))?;
        let rows = stmt.query_map(rusqlite::params![pattern], fact_from_row)?;
        let mut facts = Vec::new();
        for row in rows {
            facts.push(row?);
        }
        sort_facts_effective(&mut facts);
        Ok(facts)
    }

    /// Multi-term prompt recall: one FTS `OR` query with SQL `LIMIT`, so a fact
    /// matching any session keyword surfaces without N separate searches or an
    /// unbounded result set (refactor-backlog §2.1 / former P1-5).
    ///
    /// When any term is shorter than a trigram, LIKE those short terms and
    /// **union** with FTS hits (deduped) so a longer sibling hit cannot hide
    /// digram-only facts. LIKE uses escaped patterns (`ESCAPE '\\'`).
    pub fn search_facts_any(&self, terms: &[&str], limit: usize) -> anyhow::Result<Vec<Fact>> {
        let terms: Vec<&str> = terms.iter().copied().filter(|t| !t.is_empty()).collect();
        if terms.is_empty() || limit == 0 {
            return Ok(Vec::new());
        }
        let match_expr = Self::build_fts_query_or(&terms);
        let short = Self::short_like_terms(&terms);
        let fts = self.search_facts_fts(&match_expr, Some(limit))?;

        match fts {
            Some(facts) if short.is_empty() => Ok(facts),
            Some(facts) => {
                // FTS may already cover short terms, but digrams often miss
                // trigram MATCH — always LIKE short terms and merge.
                let like = self.search_facts_like_any(&short, limit)?;
                Ok(Self::merge_facts_limited(facts, like, limit))
            }
            None if short.is_empty() => {
                // FTS unavailable: full OR LIKE for all terms.
                self.search_facts_like_any(&terms, limit)
            }
            None => {
                // Prefer short-term LIKE first, then remaining long terms if
                // still under the cap.
                let like_short = self.search_facts_like_any(&short, limit)?;
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
                let like_long = self.search_facts_like_any(&long, limit)?;
                Ok(Self::merge_facts_limited(like_short, like_long, limit))
            }
        }
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

    /// Distinct predicates with row counts, highest count first (M6).
    pub fn list_predicate_counts(&self) -> anyhow::Result<Vec<(String, u64)>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT predicate, COUNT(*) AS n FROM facts
             GROUP BY predicate
             ORDER BY n DESC, predicate ASC",
        )?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)? as u64)))?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// Rewrite every row with predicate `from` to `to`, then collapse exact
    /// duplicates. Used by maintenance LLM alias merge (M6). Returns rows
    /// updated before dedup.
    pub fn rewrite_predicate(&self, from: &str, to: &str) -> anyhow::Result<u64> {
        let from = from.trim();
        let to = to.trim();
        if from.is_empty() || to.is_empty() || from == to {
            return Ok(0);
        }
        let updated = {
            let conn = self.conn();
            conn.execute(
                "UPDATE facts SET predicate = ?1 WHERE predicate = ?2",
                rusqlite::params![to, from],
            )? as u64
        };
        if updated > 0 {
            self.cache_invalidate_all_facts();
            self.cache_invalidate_embeddings(crate::embeddings::entity_kind::FACT);
            // Drop the connection before dedup — `conn()` is a mutex pool.
            let _ = self.dedup_facts()?;
        }
        Ok(updated)
    }

    pub fn dedup_facts(&self) -> anyhow::Result<u64> {
        // P1-6: only load rows that participate in duplicate groups (not the
        // full table), merge tags onto the keeper, then collapse with the same
        // window DELETE used by migrate_v2.
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!(
            "SELECT {FACT_COLS} FROM facts
             WHERE (subject, predicate, object) IN (
                 SELECT subject, predicate, object FROM facts
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
        for (tags, id) in &keeper_updates {
            let tag_refs: Vec<&str> = tags.iter().map(|s| s.as_str()).collect();
            conn.execute(
                "UPDATE facts SET tags = ?1 WHERE id = ?2",
                rusqlite::params![serialize_tags(&tag_refs), id],
            )?;
        }

        let deleted = if had_duplicate_groups {
            conn.execute(
                "DELETE FROM facts
                 WHERE id NOT IN (
                     SELECT id FROM (
                         SELECT id, ROW_NUMBER() OVER (
                             PARTITION BY subject, predicate, object
                             ORDER BY confidence DESC, created_at DESC
                         ) AS rn FROM facts
                     ) WHERE rn = 1
                 )",
                [],
            )? as u64
        } else {
            0
        };
        if deleted > 0 || !keeper_updates.is_empty() {
            self.cache_invalidate_all_facts();
            self.cache_invalidate_embeddings(crate::embeddings::entity_kind::FACT);
        }
        Ok(deleted)
    }

    /// Remove facts whose predicate or object looks like a credential. Called
    /// during fact maintenance so secrets accidentally extracted in the past
    /// are purged from the database rather than merely hidden from prompts.
    pub fn delete_sensitive_facts(&self) -> anyhow::Result<u64> {
        // P1-6: single bulk DELETE mirroring is_sensitive_predicate / object.
        let conn = self.conn();
        let deleted = conn.execute(
            "DELETE FROM facts WHERE
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
             OR lower(trim(object)) LIKE 'xoxb-%'
             OR lower(trim(object)) LIKE 'aiza%'
             OR lower(trim(object)) LIKE 'bearer %'
             OR instr(lower(object), 'api_key=') > 0
             OR instr(lower(object), 'apikey=') > 0",
            [],
        )? as u64;
        if deleted > 0 {
            self.cache_invalidate_all_facts();
            self.cache_invalidate_embeddings(crate::embeddings::entity_kind::FACT);
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
        // P1-6: SQL prefilter by grace-period cutoff (RFC3339 strings sort
        // lexicographically), then exact `fact_effective_confidence` on that
        // candidate set — avoids pulling fresh rows that cannot flush yet.
        let cutoff = (Utc::now() - chrono::Duration::days(1)).to_rfc3339();
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!(
            "SELECT {FACT_COLS} FROM facts
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
            &format!("DELETE FROM facts WHERE id IN ({placeholders})"),
            rusqlite::params_from_iter(stale_ids.iter().map(|s| s.as_str())),
        )? as u64;
        self.cache_invalidate_all_facts();
        self.cache_invalidate_embeddings(crate::embeddings::entity_kind::FACT);
        Ok(count)
    }

    /// Clear `source_ref.message_id` when the referenced message **and**
    /// episode no longer exist; keep the snippet so “why we remember” still
    /// works (L2 / P2-9). Compaction-summary facts (M3) store the episode
    /// `msg-*` id, which lives in `memory_episodes` not `messages`.
    pub fn cleanup_orphan_source_refs(&self) -> anyhow::Result<u64> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT id, source_ref FROM facts
             WHERE source_ref IS NOT NULL
               AND json_extract(source_ref, '$.message_id') IS NOT NULL
               AND json_extract(source_ref, '$.message_id') != ''
               AND NOT EXISTS (
                   SELECT 1 FROM messages
                   WHERE id = json_extract(facts.source_ref, '$.message_id')
               )
               AND NOT EXISTS (
                   SELECT 1 FROM memory_episodes
                   WHERE id = json_extract(facts.source_ref, '$.message_id')
               )",
        )?;
        let orphans: Vec<(String, String)> = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<Result<Vec<_>, _>>()?;
        if orphans.is_empty() {
            return Ok(0);
        }
        let mut updated = 0u64;
        for (id, raw) in orphans {
            let Some(mut refer) = parse_source_ref(Some(raw)) else {
                continue;
            };
            refer.message_id.clear();
            conn.execute(
                "UPDATE facts SET source_ref = ?1 WHERE id = ?2",
                rusqlite::params![serialize_source_ref(Some(&refer)), id],
            )?;
            updated += 1;
        }
        if updated > 0 {
            self.cache_invalidate_all_facts();
        }
        Ok(updated)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        FactSourceRef, UpsertOutcome, fact_effective_confidence, is_single_valued_predicate,
        is_volatile_predicate,
    };
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
        // P2-11: bare `company` collapses onto works_at (single-valued).
        db.set_user_fact("user", "company", "Globex", &["identity"])
            .unwrap();
        let facts = db.get_facts("user").unwrap();
        assert_eq!(facts.len(), 1);
        assert_eq!(facts[0].predicate, "works_at");
        assert_eq!(facts[0].object, "Globex");
        assert!(is_single_valued_predicate("works_at"));
        assert!(!is_single_valued_predicate("company")); // dead alias removed
        assert!(!is_volatile_predicate("workspace")); // dead alias removed
        assert!(is_volatile_predicate("project_path"));
    }

    #[test]
    fn test_search_facts_long_empty_fts_skips_like() {
        let db = create_db();
        // Plant a row that would match a naive whole-query LIKE on a long
        // substring that FTS (AND of whitespace tokens) will not hit.
        db.insert_fact(
            "user",
            "notes",
            "zzzzlongtokennevermatched",
            "inferred",
            0.9,
            &[],
        )
        .unwrap();
        // Multi-word long query: empty FTS must not fall back to LIKE (P2-14).
        let results = db
            .search_facts("zzzzlongtokennevermatched totallyunrelated")
            .unwrap();
        assert!(
            results.is_empty(),
            "long empty-FTS must not LIKE-scan; got {results:?}"
        );
        // Short digram still may LIKE.
        db.insert_fact("user", "likes", "Go", "inferred", 0.8, &[])
            .unwrap();
        let short = db.search_facts("Go").unwrap();
        assert!(short.iter().any(|f| f.object == "Go"));
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

    #[test]
    fn test_list_predicate_counts_and_rewrite() {
        let db = create_db();
        // Bypass normalize_predicate to simulate legacy free-form rows (M6).
        let conn = db.conn();
        conn.execute(
            "INSERT INTO facts (id, subject, predicate, object, source, confidence, created_at)
             VALUES ('fact-a', 'user', 'fav_lang', 'Rust', 'inferred', 0.8, '2026-01-01T00:00:00Z'),
                    ('fact-b', 'user', 'language', 'Rust', 'inferred', 0.9, '2026-01-01T00:00:01Z'),
                    ('fact-c', 'user', 'fav_lang', 'Go', 'inferred', 0.7, '2026-01-01T00:00:02Z')",
            [],
        )
        .unwrap();
        drop(conn);
        db.cache_invalidate_all_facts();

        let counts = db.list_predicate_counts().unwrap();
        assert!(
            counts
                .iter()
                .any(|(p, n)| p == "fav_lang" && *n == 2)
        );
        let rewritten = db.rewrite_predicate("fav_lang", "language").unwrap();
        assert_eq!(rewritten, 2);
        let remaining = db.list_facts().unwrap();
        assert!(remaining.iter().all(|f| f.predicate == "language"));
        // Identical (subject, predicate, object=Rust) collapsed by dedup.
        assert_eq!(
            remaining
                .iter()
                .filter(|f| f.object == "Rust")
                .count(),
            1
        );
        assert!(remaining.iter().any(|f| f.object == "Go"));
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
    fn test_cleanup_orphan_source_refs_clears_message_id_keeps_snippet() {
        let db = create_db();
        let session = db.create_session("t", "").unwrap();
        let msg = db
            .add_message(&session.id, "user", "I like Rust a lot", Some("text"), None)
            .unwrap();
        let live = FactSourceRef::from_message(&msg.id, "I like Rust a lot");
        let orphan = FactSourceRef {
            message_id: "msg-deadbeefdeadbeefdeadbeefdeadbeef".into(),
            snippet: "orphan snippet".into(),
        };
        db.upsert_fact(
            "user",
            "likes",
            "Rust",
            "inferred",
            0.9,
            &["preference"],
            Some(&live),
        )
        .unwrap();
        db.upsert_fact(
            "user",
            "likes",
            "Go",
            "inferred",
            0.8,
            &["preference"],
            Some(&orphan),
        )
        .unwrap();

        let cleared = db.cleanup_orphan_source_refs().unwrap();
        assert_eq!(cleared, 1);
        let facts = db.get_facts("user").unwrap();
        let rust = facts.iter().find(|f| f.object == "Rust").unwrap();
        assert_eq!(rust.source_ref.as_ref().unwrap().message_id, msg.id);
        let go = facts.iter().find(|f| f.object == "Go").unwrap();
        let go_ref = go.source_ref.as_ref().unwrap();
        assert!(go_ref.message_id.is_empty());
        assert_eq!(go_ref.snippet, "orphan snippet");
    }

    #[test]
    fn test_cleanup_orphan_source_refs_keeps_episode_ids() {
        let db = create_db();
        let session = db.create_session("t", "").unwrap();
        let episode_id = haven_common::types::new_id("msg");
        db.add_episode_with_id(&session.id, "User prefers dark theme", &episode_id)
            .unwrap();
        let from_episode = FactSourceRef::from_message(&episode_id, "User prefers dark theme");
        db.upsert_fact(
            "user",
            "theme",
            "dark",
            "inferred",
            0.9,
            &["preference"],
            Some(&from_episode),
        )
        .unwrap();
        let cleared = db.cleanup_orphan_source_refs().unwrap();
        assert_eq!(cleared, 0);
        let facts = db.get_facts("user").unwrap();
        let theme = facts.iter().find(|f| f.predicate == "theme").unwrap();
        assert_eq!(
            theme.source_ref.as_ref().unwrap().message_id,
            episode_id
        );
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
    fn test_get_facts_limited_orders_by_confidence() {
        let db = create_db();
        db.insert_fact("user", "likes", "Low", "inferred", 0.4, &["preference"])
            .unwrap();
        db.insert_fact("user", "likes", "High", "inferred", 0.95, &["preference"])
            .unwrap();
        db.insert_fact("user", "likes", "Mid", "inferred", 0.7, &["preference"])
            .unwrap();
        db.insert_fact("other", "likes", "Other", "inferred", 1.0, &["preference"])
            .unwrap();

        let top = db.get_facts_limited("user", 2).unwrap();
        assert_eq!(top.len(), 2);
        assert_eq!(top[0].object, "High");
        assert_eq!(top[1].object, "Mid");
        assert!(db.get_facts_limited("user", 0).unwrap().is_empty());
    }

    #[test]
    fn test_search_facts_any_or_with_limit() {
        let db = create_db();
        db.insert_fact("user", "uses", "VSCode", "user", 0.9, &["preference"])
            .unwrap();
        db.insert_fact("user", "uses", "IntelliJ", "user", 0.7, &["preference"])
            .unwrap();
        db.insert_fact("user", "likes", "Coffee", "user", 0.8, &["preference"])
            .unwrap();
        db.insert_fact(
            "haven",
            "project_path",
            "D:/Workspace/Haven",
            "inferred",
            0.85,
            &["workspace"],
        )
        .unwrap();

        // OR: either term is enough (unlike search_facts AND).
        let results = db.search_facts_any(&["VSCode", "Coffee"], 10).unwrap();
        let objects: Vec<&str> = results.iter().map(|f| f.object.as_str()).collect();
        assert!(objects.contains(&"VSCode"));
        assert!(objects.contains(&"Coffee"));
        assert!(!objects.contains(&"IntelliJ"));

        // Cross-subject + LIMIT.
        let limited = db
            .search_facts_any(&["VSCode", "Haven", "Coffee"], 2)
            .unwrap();
        assert_eq!(limited.len(), 2);

        assert!(db.search_facts_any(&[], 5).unwrap().is_empty());
        assert!(db.search_facts_any(&["VSCode"], 0).unwrap().is_empty());
    }

    #[test]
    fn test_search_facts_any_unions_short_term_like_with_fts_hits() {
        let db = create_db();
        // Longer term hits FTS; digram-only fact must still surface via LIKE union.
        db.insert_fact(
            "user",
            "uses",
            "VSCode editor",
            "inferred",
            0.9,
            &["preference"],
        )
        .unwrap();
        db.insert_fact("habit", "likes", "咖啡", "inferred", 0.7, &["preference"])
            .unwrap();

        let results = db.search_facts_any(&["VSCode", "咖啡"], 10).unwrap();
        let objects: Vec<&str> = results.iter().map(|f| f.object.as_str()).collect();
        assert!(
            objects.contains(&"VSCode editor"),
            "FTS hit must remain; got {objects:?}"
        );
        assert!(
            objects.contains(&"咖啡"),
            "2-char CJK digram must LIKE-union even when FTS already hit; got {objects:?}"
        );
    }

    #[test]
    fn test_search_facts_like_escapes_underscore_metachar() {
        let db = create_db();
        db.insert_fact("user", "likes", "xaZy", "inferred", 0.9, &["preference"])
            .unwrap();
        db.insert_fact("user", "likes", "xa_y", "inferred", 0.8, &["preference"])
            .unwrap();

        // 2-char term forces LIKE path. Without ESCAPE, `%a_%` matches `xaZy`.
        let results = db.search_facts_any(&["a_"], 10).unwrap();
        assert_eq!(results.len(), 1, "got {:?}", results);
        assert_eq!(results[0].object, "xa_y");

        // Tool search_facts also escapes the whole-query LIKE fallback.
        let via_tool = db.search_facts("xa_y").unwrap();
        assert!(via_tool.iter().any(|f| f.object == "xa_y"));
        assert!(!via_tool.iter().any(|f| f.object == "xaZy"));
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
