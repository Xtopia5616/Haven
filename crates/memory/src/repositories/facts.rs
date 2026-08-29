use crate::db::Database;
use chrono::{DateTime, Utc};
use std::collections::{HashMap, HashSet};

use super::fact_graph::FactGraph;

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
/// Rehydrated from provenance_* columns on `memory_edges` for traceability
/// and contradiction checks.
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
pub(crate) const FACT_COLS: &str = "id, subject, predicate, object, source, confidence, tags, created_at, mention_count, last_seen_at, provenance_item_id, provenance_record_id, provenance_snippet, durability";

/// Aliased variant for queries that prefix columns with a table alias
/// (FTS join).
const FACT_COLS_ALIASED: &str = "f.id, f.subject, f.predicate, f.object, f.source, f.confidence, f.tags, f.created_at, f.mention_count, f.last_seen_at, f.provenance_item_id, f.provenance_record_id, f.provenance_snippet, f.durability";

/// Map a rusqlite Row (with the standard 12-column SELECT order) to a Fact.
/// Shared by all query methods to avoid drift when columns change.
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
) -> Option<FactSourceRef> {
    let message_id = item_id.or(record_id).unwrap_or_default();
    let snippet = snippet.unwrap_or_default();
    if message_id.is_empty() && snippet.is_empty() {
        None
    } else {
        Some(FactSourceRef {
            message_id,
            snippet,
        })
    }
}

/// Stable identity predicates: never decay and never get auto-corrected away
/// by a contradicting inference. Shared with the SQL scanner list so the two
/// cannot drift.
const IDENTITY_PREDICATES: &[&str] = &[
    "name", "birthday", "email", "phone", "city", "country", "timezone",
];

/// Non-identity single-valued predicates (canonical names only). Combined with
/// [`IDENTITY_PREDICATES`] for upsert demotion and the X5 contradiction scan.
const SINGLE_VALUED_NON_IDENTITY: &[&str] = &[
    "project_path",
    "works_at",
    "action",
    "role",
    "shell",
    "os",
    "location",
    "address",
    "language",
    "verbosity",
];

/// Predicates describing stable identity attributes: they never decay and
/// never get auto-corrected away by a contradicting inference.
pub fn is_identity_predicate(predicate: &str) -> bool {
    let p = predicate.to_ascii_lowercase();
    IDENTITY_PREDICATES.iter().any(|x| *x == p)
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
    let p = predicate.to_ascii_lowercase();
    IDENTITY_PREDICATES.iter().any(|x| *x == p)
        || SINGLE_VALUED_NON_IDENTITY.iter().any(|x| *x == p)
}

/// All single-valued predicates for SQL `IN (...)` filters — same set as
/// [`is_single_valued_predicate`].
fn all_single_valued_predicates() -> impl Iterator<Item = &'static str> {
    IDENTITY_PREDICATES
        .iter()
        .chain(SINGLE_VALUED_NON_IDENTITY.iter())
        .copied()
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
        FactGraph::new(self).insert(subject, predicate, object, source, confidence, tags)
    }

    /// Insert a fact with an optional message reference and durability rating.
    /// The predicate is normalized before it reaches the graph writer.
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
        FactGraph::new(self).insert_with_source_ref(
            subject, predicate, object, source, confidence, tags, source_ref, durability,
        )
    }

    /// Store a fact explicitly stated by the user. User-stated facts are
    /// authoritative; single-valued predicates replace prior values and
    /// repeated statements reinforce the existing row.
    pub fn set_user_fact(
        &self,
        subject: &str,
        predicate: &str,
        object: &str,
        tags: &[&str],
    ) -> anyhow::Result<Fact> {
        FactGraph::new(self).set_user(subject, predicate, object, tags)
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
        FactGraph::new(self).delete_by_triple(subject, predicate, object)
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
        FactGraph::new(self).ensure(subject, predicate, object, source, confidence, tags)
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
        FactGraph::new(self).upsert(
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
        FactGraph::new(self).upsert(
            subject, predicate, object, source, confidence, tags, source_ref, durability,
        )
    }

    /// Fetch a single fact by id. Used by the prompt builder to resolve
    /// vector-recall hits (`search_embeddings` returns entity ids) back into
    /// full facts for ranking and rendering.
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
        let key = format!("_facts_{}", subject);
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
        let edge = crate::embeddings::fts_kind::EDGE;
        let (fts_sql, bind_limit) = if let Some(lim) = limit {
            (
                format!(
                    "SELECT {FACT_COLS_ALIASED}
                     FROM memory_edges f
                     JOIN memory_fts ON memory_fts.entity_id = f.id
                       AND memory_fts.entity_type = '{edge}'
                     WHERE memory_fts MATCH ?1
                     ORDER BY bm25(memory_fts)
                     LIMIT ?2"
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
                     ORDER BY bm25(memory_fts)"
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
            "SELECT {FACT_COLS} FROM memory_edges
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
            "SELECT {FACT_COLS} FROM memory_edges
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
    /// unbounded result set.
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

    pub fn delete_fact(&self, id: &str) -> anyhow::Result<()> {
        FactGraph::new(self).delete_by_id(id)
    }

    /// Distinct predicates with row counts, highest count first (M6).
    pub fn list_predicate_counts(&self) -> anyhow::Result<Vec<(String, u64)>> {
        let conn = self.conn();
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
                "UPDATE memory_edges SET predicate = ?1 WHERE predicate = ?2",
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
                "UPDATE memory_edges SET tags = ?1 WHERE id = ?2",
                rusqlite::params![serialize_tags(&tag_refs), id],
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
        self.cache_invalidate_all_facts();
        self.cache_invalidate_embeddings(crate::embeddings::entity_kind::FACT);
        Ok(count)
    }

    /// Normalize empty provenance_record_id strings. Item provenance is
    /// enforced by FK (`ON DELETE SET NULL`); opaque transcript record ids are
    /// intentional stable refs and are left alone (no messages-table scan).
    pub fn cleanup_orphan_source_refs(&self) -> anyhow::Result<u64> {
        let conn = self.conn();
        let n = conn.execute(
            "UPDATE memory_edges
             SET provenance_record_id = NULL
             WHERE provenance_record_id IS NOT NULL
               AND TRIM(provenance_record_id) = ''",
            [],
        )? as u64;
        if n > 0 {
            self.cache_invalidate_all_facts();
        }
        Ok(n)
    }

    /// Maintenance contradiction engine (X5): scan polarity and single-valued
    /// conflicts that slipped past upsert (`insert_fact` / bulk paths), pick a
    /// keeper with the same user>inferred / confidence / recency rules, and
    /// demote losers once (`confidence *= 0.5`). SPO text and `source_ref`
    /// provenance are preserved as evidence. Returns rows demoted.
    ///
    /// Losers older than [`CONTRADICTION_DEMOTE_MAX_AGE_DAYS`] (by
    /// `last_seen_at`/`created_at`) are left alone so a maintenance pass
    /// cannot mass-demote historical edges into the subsequent
    /// `flush_low_confidence` delete window. Older residuals still surface via
    /// [`Self::list_ambiguous_contradictions`] for optional LLM arbitration.
    pub fn resolve_contradictions(&self) -> anyhow::Result<u64> {
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

    /// Remaining live conflict groups for optional LLM arbitration (X5).
    /// After the rule pass, any polarity / single-valued group that still has
    /// ≥2 facts at/above the live floor is surfaced (near-synonym objects,
    /// both-user changes, residual polarity). `source_ref` snippets travel
    /// with each fact for evidence.
    pub fn list_ambiguous_contradictions(&self) -> anyhow::Result<Vec<ContradictionCandidate>> {
        let mut out = Vec::new();
        for mut group in self.collect_contradiction_groups()? {
            group
                .facts
                .retain(|f| fact_effective_confidence(f) >= CONTRADICTION_LIVE_FLOOR);
            if group.facts.len() >= 2 {
                out.push(group);
            }
        }
        Ok(out)
    }

    /// Halve (or scale by `factor`) confidence for specific fact ids. Used by
    /// the maintenance LLM arbitrator after gated proposals. Preserves SPO and
    /// provenance. Returns how many rows were updated.
    pub fn demote_fact_ids(&self, ids: Vec<String>) -> anyhow::Result<u64> {
        self.demote_fact_ids_by_factor(ids, CONTRADICTION_DEMOTE_FACTOR)
    }

    fn demote_fact_ids_by_factor(&self, ids: Vec<String>, factor: f64) -> anyhow::Result<u64> {
        if ids.is_empty() {
            return Ok(0);
        }
        let factor = factor.clamp(0.0, 1.0);
        let conn = self.conn();
        let placeholders = vec!["?"; ids.len()].join(",");
        // params: factor first, then ids (all anonymous `?` binders).
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
            self.cache_invalidate_all_facts();
            self.cache_invalidate_embeddings(crate::embeddings::entity_kind::FACT);
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
        let conn = self.conn();
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
            .get_facts_by_ids(&unique_ids)?
            .into_iter()
            .map(|f| (f.id.clone(), f))
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
        let conn = self.conn();
        let placeholders = vec!["?"; predicates.len()].join(",");
        // One scan: all live single-valued rows, then group in Rust where
        // distinct objects collide (avoids N+1 prepare per subject/predicate).
        let sql = format!(
            "SELECT {FACT_COLS} FROM memory_edges
             WHERE lower(predicate) IN ({placeholders})
               AND confidence >= ?"
        );
        let mut stmt = conn.prepare(&sql)?;
        let mut params: Vec<rusqlite::types::Value> =
            predicates.iter().map(|p| (*p).to_string().into()).collect();
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
                .map(|f| f.object.to_ascii_lowercase())
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

/// Live-floor for maintenance contradiction scans (X5). Below this, upsert
/// demotion / flush already treat the fact as inactive in the prompt.
pub const CONTRADICTION_LIVE_FLOOR: f64 = 0.4;
/// Rule-engine demotion age cap (X5). Older losers are not mutated so a
/// maintenance pass cannot push historical edges under the flush floor /
/// rewrite unbounded ancient conflicts on first upgrade.
pub const CONTRADICTION_DEMOTE_MAX_AGE_DAYS: i64 = 2;

/// True when the fact was last seen (or created) within the X5 demote age cap.
pub fn fact_within_demote_age(fact: &Fact, now: DateTime<Utc>) -> bool {
    let ts = fact.last_seen_at.as_deref().unwrap_or(&fact.created_at);
    match DateTime::parse_from_rfc3339(ts) {
        Ok(dt) => {
            now.signed_duration_since(dt.with_timezone(&Utc))
                <= chrono::Duration::days(CONTRADICTION_DEMOTE_MAX_AGE_DAYS)
        }
        // Unparseable timestamps: skip demotion rather than risk mutating
        // opaque historical rows into the flush window.
        Err(_) => false,
    }
}

/// Opposite polarity predicate (`likes` ↔ `dislikes`), shared by upsert and
/// the maintenance scanner so the pair cannot drift.
pub fn polarity_opposite(predicate: &str) -> Option<&'static str> {
    match predicate {
        "likes" => Some("dislikes"),
        "dislikes" => Some("likes"),
        _ => None,
    }
}

/// Shared demote strength for upsert corrections and the X5 engine.
pub const CONTRADICTION_DEMOTE_FACTOR: f64 = 0.5;

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

/// Pick the keeper and the losers for a conflict group. Returns `None` when
/// the group has fewer than two facts.
///
/// - **Polarity** (aligned with upsert): user > inferred, then newest
///   observation (`last_seen_at`/`created_at`), then confidence / mentions.
/// - **Single-valued**: user > inferred, then effective confidence, mentions,
///   then recency.
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
    use super::{
        FactSourceRef, UpsertOutcome, fact_effective_confidence, is_single_valued_predicate,
        is_volatile_predicate,
    };
    use crate::Database;
    use chrono::Utc;

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
            "INSERT INTO memory_edges (id, subject, predicate, object, source, confidence, created_at)
             VALUES ('fact-a', 'user', 'fav_lang', 'Rust', 'inferred', 0.8, '2026-01-01T00:00:00Z'),
                    ('fact-b', 'user', 'language', 'Rust', 'inferred', 0.9, '2026-01-01T00:00:01Z'),
                    ('fact-c', 'user', 'fav_lang', 'Go', 'inferred', 0.7, '2026-01-01T00:00:02Z')",
            [],
        )
        .unwrap();
        drop(conn);
        db.cache_invalidate_all_facts();

        let counts = db.list_predicate_counts().unwrap();
        assert!(counts.iter().any(|(p, n)| p == "fav_lang" && *n == 2));
        let rewritten = db.rewrite_predicate("fav_lang", "language").unwrap();
        assert_eq!(rewritten, 2);
        let remaining = db.list_facts().unwrap();
        assert!(remaining.iter().all(|f| f.predicate == "language"));
        // Identical (subject, predicate, object=Rust) collapsed by dedup.
        assert_eq!(remaining.iter().filter(|f| f.object == "Rust").count(), 1);
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
            "UPDATE memory_edges SET created_at = ?1, last_seen_at = ?1",
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
    fn test_cleanup_orphan_source_refs_clears_empty_record_id() {
        let db = create_db();
        let fact = db
            .insert_fact("user", "likes", "Go", "inferred", 0.8, &["preference"])
            .unwrap();
        {
            let conn = db.conn();
            conn.execute(
                "UPDATE memory_edges SET provenance_record_id = '   ' WHERE id = ?1",
                rusqlite::params![fact.id],
            )
            .unwrap();
        }
        let cleared = db.cleanup_orphan_source_refs().unwrap();
        assert_eq!(cleared, 1);
        let got = db.get_fact_by_id(&fact.id).unwrap().unwrap();
        assert!(got.source_ref.is_none() || got.source_ref.as_ref().unwrap().message_id.is_empty());
    }

    #[test]
    fn test_cleanup_orphan_source_refs_keeps_transcript_and_item_ids() {
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
        let transcript = FactSourceRef {
            message_id: "msg-deadbeefdeadbeefdeadbeefdeadbeef".into(),
            snippet: "opaque transcript".into(),
        };
        db.upsert_fact(
            "user",
            "likes",
            "Go",
            "inferred",
            0.8,
            &["preference"],
            Some(&transcript),
        )
        .unwrap();
        let cleared = db.cleanup_orphan_source_refs().unwrap();
        assert_eq!(cleared, 0);
        let facts = db.get_facts("user").unwrap();
        let theme = facts.iter().find(|f| f.predicate == "theme").unwrap();
        assert_eq!(theme.source_ref.as_ref().unwrap().message_id, episode_id);
        let go = facts.iter().find(|f| f.object == "Go").unwrap();
        assert_eq!(
            go.source_ref.as_ref().unwrap().message_id,
            transcript.message_id
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
            "UPDATE memory_edges SET created_at = ?1, last_seen_at = ?1",
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
            "UPDATE memory_edges SET created_at = '2024-01-01T00:00:00Z', last_seen_at = '2024-01-01T00:00:00Z'",
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
            "UPDATE memory_edges SET created_at = '2024-01-01T00:00:00Z', last_seen_at = '2024-01-01T00:00:00Z'
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

    #[test]
    fn test_resolve_contradictions_polarity_user_beats_inferred() {
        let db = create_db();
        // Bypass upsert demotion: insert_fact stores both sides raw (X5 catch-up).
        let like_ref = FactSourceRef {
            message_id: "msg-like".into(),
            snippet: "I love Rust".into(),
        };
        let dislike_ref = FactSourceRef {
            message_id: "msg-dislike".into(),
            snippet: "Rust is awful".into(),
        };
        db.insert_fact_with_source_ref(
            "user",
            "likes",
            "Rust",
            "inferred",
            0.9,
            &["preference"],
            Some(&like_ref),
            1.0,
        )
        .unwrap();
        db.insert_fact_with_source_ref(
            "user",
            "dislikes",
            "Rust",
            "user",
            1.0,
            &["preference"],
            Some(&dislike_ref),
            1.0,
        )
        .unwrap();
        let demoted = db.resolve_contradictions().unwrap();
        assert_eq!(demoted, 1);
        let facts = db.get_facts("user").unwrap();
        let likes = facts.iter().find(|f| f.predicate == "likes").unwrap();
        let dislikes = facts.iter().find(|f| f.predicate == "dislikes").unwrap();
        assert!(
            (likes.confidence - 0.45).abs() < 1e-9,
            "inferred likes must be demoted, got {}",
            likes.confidence
        );
        assert!((dislikes.confidence - 1.0).abs() < 1e-9);
        // Provenance survives demotion as evidence.
        assert_eq!(likes.source_ref.as_ref().unwrap().snippet, "I love Rust");
        assert_eq!(
            dislikes.source_ref.as_ref().unwrap().snippet,
            "Rust is awful"
        );
    }

    #[test]
    fn test_resolve_contradictions_single_valued_keeps_user() {
        let db = create_db();
        db.insert_fact(
            "user",
            "project_path",
            "/old/path",
            "inferred",
            0.85,
            &["workspace"],
        )
        .unwrap();
        db.insert_fact(
            "user",
            "project_path",
            "/new/path",
            "user",
            1.0,
            &["workspace"],
        )
        .unwrap();
        let demoted = db.resolve_contradictions().unwrap();
        assert_eq!(demoted, 1);
        let facts = db.get_facts("user").unwrap();
        let old = facts.iter().find(|f| f.object == "/old/path").unwrap();
        let new = facts.iter().find(|f| f.object == "/new/path").unwrap();
        assert!((old.confidence - 0.425).abs() < 1e-9);
        assert!((new.confidence - 1.0).abs() < 1e-9);
    }

    #[test]
    fn test_list_ambiguous_contradictions_after_rule_demote() {
        let db = create_db();
        // High enough that one *0.5 demote still leaves both above the floor.
        db.insert_fact("user", "works_at", "Acme", "inferred", 0.95, &["work"])
            .unwrap();
        db.insert_fact("user", "works_at", "BetaCorp", "inferred", 0.92, &["work"])
            .unwrap();
        let demoted = db.resolve_contradictions().unwrap();
        assert_eq!(demoted, 1);
        let ambiguous = db.list_ambiguous_contradictions().unwrap();
        assert!(
            ambiguous.iter().any(|c| {
                c.kind == super::ContradictionKind::SingleValued && c.facts.len() >= 2
            }),
            "residual single-valued pair should stay ambiguous after one demote"
        );
    }

    #[test]
    fn test_resolve_contradictions_noop_when_clean() {
        let db = create_db();
        db.insert_fact("user", "likes", "Rust", "inferred", 0.9, &[])
            .unwrap();
        db.insert_fact("user", "likes", "Go", "inferred", 0.8, &[])
            .unwrap();
        assert_eq!(db.resolve_contradictions().unwrap(), 0);
        assert!(db.list_ambiguous_contradictions().unwrap().is_empty());
    }

    #[test]
    fn test_resolve_contradictions_skips_losers_older_than_age_cap() {
        let db = create_db();
        db.insert_fact("user", "likes", "Rust", "inferred", 0.9, &["preference"])
            .unwrap();
        db.insert_fact("user", "dislikes", "Rust", "user", 1.0, &["preference"])
            .unwrap();
        let stale = (Utc::now() - chrono::Duration::days(30)).to_rfc3339();
        {
            let conn = db.conn();
            conn.execute(
                "UPDATE memory_edges SET created_at = ?1, last_seen_at = ?1",
                rusqlite::params![stale],
            )
            .unwrap();
        }
        assert_eq!(db.resolve_contradictions().unwrap(), 0);
        let likes = db
            .get_facts("user")
            .unwrap()
            .into_iter()
            .find(|f| f.predicate == "likes")
            .unwrap();
        assert!(
            (likes.confidence - 0.9).abs() < 1e-9,
            "stale inferred likes must not be demoted, got {}",
            likes.confidence
        );
        let ambiguous = db.list_ambiguous_contradictions().unwrap();
        assert!(
            ambiguous
                .iter()
                .any(|c| c.kind == super::ContradictionKind::Polarity),
            "stale conflicts still surface for LLM arbitration"
        );
    }

    #[test]
    fn test_single_valued_predicate_list_matches_helper() {
        let listed: Vec<&str> = super::all_single_valued_predicates().collect();
        for p in &listed {
            assert!(
                is_single_valued_predicate(p),
                "scanner list entry `{p}` missing from is_single_valued_predicate"
            );
        }
        // Bidirectional: every helper-true canonical name is in the SQL list.
        for p in super::IDENTITY_PREDICATES
            .iter()
            .chain(super::SINGLE_VALUED_NON_IDENTITY.iter())
        {
            assert!(
                listed.contains(p),
                "helper predicate `{p}` missing from scanner list"
            );
        }
        // Spot-check multi-valued stay out of the scanner list.
        assert!(!listed.contains(&"likes"));
        assert!(!listed.contains(&"uses"));
    }
}
