use crate::db::Database;
use std::collections::HashSet;

use super::fact_graph::FactGraph;
use super::fact_maintenance::FactMaintenance;
pub use super::fact_maintenance::{
    CONTRADICTION_DEMOTE_MAX_AGE_DAYS, CONTRADICTION_LIVE_FLOOR, ContradictionCandidate,
    ContradictionKind, fact_within_demote_age, pick_contradiction_keeper,
};
pub use super::fact_query::fact_effective_confidence;

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
pub(crate) fn all_single_valued_predicates() -> impl Iterator<Item = &'static str> {
    IDENTITY_PREDICATES
        .iter()
        .chain(SINGLE_VALUED_NON_IDENTITY.iter())
        .copied()
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
        || o.starts_with("ghs_")
        || o.starts_with("github_pat_")
        || o.starts_with("glpat-")
        || o.starts_with("xoxb-")
        || o.starts_with("xoxp-")
        || o.starts_with("xoxa-")
        || o.starts_with("xoxr-")
        || o.starts_with("xapp-")
        || o.starts_with("npm_")
        || o.starts_with("pypi-")
        || o.starts_with("dop_v1_")
        || o.starts_with("aiza")
        || o.starts_with("akia")
        || o.starts_with("asia")
        || o.starts_with("bearer ")
        || (o.starts_with("eyj") && o.matches('.').count() >= 2)
        || (o.starts_with("-----begin") && o.contains("private key"))
        || o.contains("api_key=")
        || o.contains("apikey=")
        || (o.contains("://") && o.contains('@'))
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
        self.with_fact_write(|| {
            FactGraph::new(self).insert(subject, predicate, object, source, confidence, tags)
        })
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
        self.with_fact_write(|| {
            FactGraph::new(self).insert_with_source_ref(
                subject, predicate, object, source, confidence, tags, source_ref, durability,
            )
        })
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
        self.with_fact_write(|| FactGraph::new(self).set_user(subject, predicate, object, tags))
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
        self.with_fact_write(|| FactGraph::new(self).delete_by_triple(subject, predicate, object))
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
        self.with_fact_write(|| {
            FactGraph::new(self).ensure(subject, predicate, object, source, confidence, tags)
        })
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
        self.with_fact_write(|| {
            FactGraph::new(self).upsert(
                subject, predicate, object, source, confidence, tags, source_ref, 1.0,
            )
        })
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
        self.with_fact_write(|| {
            FactGraph::new(self).upsert(
                subject, predicate, object, source, confidence, tags, source_ref, durability,
            )
        })
    }

    pub fn delete_fact(&self, id: &str) -> anyhow::Result<()> {
        self.with_fact_write(|| FactGraph::new(self).delete_by_id(id))
    }

    /// Distinct predicates with row counts, highest count first (M6).
    pub fn list_predicate_counts(&self) -> anyhow::Result<Vec<(String, u64)>> {
        FactMaintenance::new(self).list_predicate_counts()
    }

    /// Rewrite every row with predicate `from` to `to`, then collapse exact
    /// duplicates. Used by maintenance LLM alias merge (M6). Returns rows
    /// updated before dedup.
    pub fn rewrite_predicate(&self, from: &str, to: &str) -> anyhow::Result<u64> {
        self.with_fact_write(|| FactMaintenance::new(self).rewrite_predicate(from, to))
    }

    pub fn dedup_facts(&self) -> anyhow::Result<u64> {
        self.with_fact_write(|| FactMaintenance::new(self).dedup_facts())
    }

    /// Remove facts whose predicate or object looks like a credential. Called
    /// during fact maintenance so secrets accidentally extracted in the past
    /// are purged from the database rather than merely hidden from prompts.
    pub fn delete_sensitive_facts(&self) -> anyhow::Result<u64> {
        self.with_fact_write(|| FactMaintenance::new(self).delete_sensitive_facts())
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
        self.with_fact_write(|| FactMaintenance::new(self).flush_low_confidence(threshold))
    }

    /// Normalize empty provenance_record_id strings. Item provenance is
    /// enforced by FK (`ON DELETE SET NULL`); opaque transcript record ids are
    /// intentional stable refs and are left alone (no messages-table scan).
    pub fn cleanup_orphan_source_refs(&self) -> anyhow::Result<u64> {
        self.with_fact_write(|| FactMaintenance::new(self).cleanup_orphan_source_refs())
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
        self.with_fact_write(|| FactMaintenance::new(self).resolve_contradictions())
    }

    /// Remaining live conflict groups for optional LLM arbitration (X5).
    /// After the rule pass, any polarity / single-valued group that still has
    /// ≥2 facts at/above the live floor is surfaced (near-synonym objects,
    /// both-user changes, residual polarity). `source_ref` snippets travel
    /// with each fact for evidence.
    pub fn list_ambiguous_contradictions(&self) -> anyhow::Result<Vec<ContradictionCandidate>> {
        FactMaintenance::new(self).list_ambiguous_contradictions()
    }

    /// Halve (or scale by `factor`) confidence for specific fact ids. Used by
    /// the maintenance LLM arbitrator after gated proposals. Preserves SPO and
    /// provenance. Returns how many rows were updated.
    pub fn demote_fact_ids(&self, ids: Vec<String>) -> anyhow::Result<u64> {
        self.with_fact_write(|| FactMaintenance::new(self).demote_fact_ids(ids))
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

#[cfg(test)]
mod tests {
    use super::{
        FactSourceRef, UpsertOutcome, fact_effective_confidence, is_sensitive_object,
        is_single_valued_predicate, is_volatile_predicate,
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
    fn test_sensitive_object_covers_common_credential_shapes() {
        for value in [
            "AKIAIOSFODNN7EXAMPLE",
            "glpat-abc123",
            "github_pat_abc123",
            "xoxp-123",
            "npm_abc123",
            "eyJhbGciOiJIUzI1NiJ9.payload.signature",
            "-----BEGIN PRIVATE KEY-----",
            "https://user:password@example.test/path",
        ] {
            assert!(is_sensitive_object(value), "not detected: {value}");
        }
        assert!(!is_sensitive_object("Rust programming language"));
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
    fn test_source_ref_redacts_sensitive_snippet_before_persistence() {
        let db = create_db();
        let source_ref = FactSourceRef {
            message_id: "m-secret".into(),
            snippet: "password=hunter2".into(),
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

        let stored = db.get_facts("user").unwrap();
        assert_eq!(stored[0].source_ref.as_ref().unwrap().snippet, "[redacted]");
        let raw: String = db
            .conn()
            .query_row(
                "SELECT provenance_snippet FROM memory_edges WHERE id = ?1",
                rusqlite::params![stored[0].id],
                |row| row.get(0),
            )
            .unwrap();
        assert!(!raw.contains("hunter2"));
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

        // The normal FTS path treats the underscore literally as well.
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
