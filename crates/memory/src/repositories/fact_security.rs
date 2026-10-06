//! Shared sensitivity rules for fact admission, recall, and maintenance purge.
//!
//! Keep Rust filtering and the bulk SQL cleanup generated from the same rule
//! lists so changes cannot silently make the purge broader than detection.

const SENSITIVE_PREDICATE_KEYWORDS: &[&str] = &[
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

const SENSITIVE_OBJECT_PREFIXES: &[&str] = &[
    "sk-",
    "tvly-",
    "ghp_",
    "gho_",
    "ghs_",
    "github_pat_",
    "glpat-",
    "xoxb-",
    "xoxp-",
    "xoxa-",
    "xoxr-",
    "xapp-",
    "npm_",
    "pypi-",
    "dop_v1_",
    "aiza",
    "akia",
    "asia",
    "bearer ",
];

const SENSITIVE_OBJECT_MARKERS: &[&str] = &["api_key=", "apikey="];
const JWT_PREFIX: &str = "eyj";
const JWT_MIN_DOTS: usize = 2;
const PEM_BEGIN_PREFIX: &str = "-----begin";
const PEM_PRIVATE_KEY_MARKER: &str = "private key";
const CREDENTIAL_URL_SCHEME_MARKER: &str = "://";
const CREDENTIAL_URL_USER_MARKER: &str = "@";

// SQLite's default trim set is only U+0020, while Rust `str::trim` follows
// Unicode White_Space. Keep the SQL expression aligned with that Rust rule.
const SQL_TRIM_WHITESPACE_CHARS: &str = "char(9) || char(10) || char(11) || char(12) || \
    char(13) || char(32) || char(133) || char(160) || char(5760) || \
    char(8192) || char(8193) || char(8194) || char(8195) || char(8196) || \
    char(8197) || char(8198) || char(8199) || char(8200) || char(8201) || \
    char(8202) || char(8232) || char(8233) || char(8239) || char(8287) || \
    char(12288)";

/// Predicate names that must never be stored as (or shown from) user facts:
/// API keys, tokens, passwords and other credentials.
pub fn is_sensitive_predicate(predicate: &str) -> bool {
    let predicate = predicate.to_ascii_lowercase();
    SENSITIVE_PREDICATE_KEYWORDS
        .iter()
        .any(|keyword| predicate.contains(keyword))
}

/// Object values that look like credentials even when the predicate is not
/// obviously sensitive (defense in depth: covers secrets the LLM happened to
/// store under an innocent predicate).
pub fn is_sensitive_object(object: &str) -> bool {
    let object = object.trim().to_ascii_lowercase();
    SENSITIVE_OBJECT_PREFIXES
        .iter()
        .any(|prefix| object.starts_with(prefix))
        || (object.starts_with(JWT_PREFIX) && object.matches('.').count() >= JWT_MIN_DOTS)
        || (object.starts_with(PEM_BEGIN_PREFIX) && object.contains(PEM_PRIVATE_KEY_MARKER))
        || SENSITIVE_OBJECT_MARKERS
            .iter()
            .any(|marker| object.contains(marker))
        || contains_credential_url(&object)
}

fn contains_credential_url(object: &str) -> bool {
    object
        .find(CREDENTIAL_URL_SCHEME_MARKER)
        .is_some_and(|scheme_start| {
            object[scheme_start + CREDENTIAL_URL_SCHEME_MARKER.len()..]
                .contains(CREDENTIAL_URL_USER_MARKER)
        })
}

/// Free-text provenance / snippets: treat as sensitive when they look like
/// credential *objects* **or** contain credential *keywords* (e.g.
/// "password is …", "token=…") that `is_sensitive_object` alone would miss.
pub fn is_sensitive_text(text: &str) -> bool {
    if is_sensitive_object(text) {
        return true;
    }
    let text = text.to_ascii_lowercase();
    SENSITIVE_PREDICATE_KEYWORDS
        .iter()
        .any(|keyword| text.contains(keyword))
}

/// Build the SQLite predicate used by the data-purge path from the same lists
/// as the Rust detectors. Prefixes use exact `substr` comparison: `_` in a
/// credential prefix is a literal underscore, never a `LIKE` wildcard.
pub(crate) fn sensitive_fact_where_sql() -> String {
    let object = format!("lower(trim(object, {SQL_TRIM_WHITESPACE_CHARS}))");
    let mut clauses = Vec::new();
    clauses.extend(
        SENSITIVE_PREDICATE_KEYWORDS
            .iter()
            .map(|keyword| format!("instr(lower(predicate), '{keyword}') > 0")),
    );
    clauses.extend(
        SENSITIVE_OBJECT_PREFIXES
            .iter()
            .map(|prefix| format!("substr({object}, 1, {}) = '{prefix}'", prefix.len())),
    );
    clauses.extend(
        SENSITIVE_OBJECT_MARKERS
            .iter()
            .map(|marker| format!("instr({object}, '{marker}') > 0")),
    );
    clauses.push(format!(
        "(substr({object}, 1, {}) = '{JWT_PREFIX}' AND \
         length({object}) - length(replace({object}, '.', '')) >= {JWT_MIN_DOTS})",
        JWT_PREFIX.len()
    ));
    clauses.push(format!(
        "(substr({object}, 1, {}) = '{PEM_BEGIN_PREFIX}' AND \
         instr({object}, '{PEM_PRIVATE_KEY_MARKER}') > 0)",
        PEM_BEGIN_PREFIX.len()
    ));
    let scheme_position = format!("instr({object}, '{CREDENTIAL_URL_SCHEME_MARKER}')");
    clauses.push(format!(
        "({scheme_position} > 0 AND \
         instr(substr({object}, {scheme_position} + {}), '{CREDENTIAL_URL_USER_MARKER}') > 0)",
        CREDENTIAL_URL_SCHEME_MARKER.len()
    ));
    format!("({})", clauses.join("\n OR "))
}

#[cfg(test)]
mod tests {
    use super::{
        SENSITIVE_OBJECT_PREFIXES, SENSITIVE_PREDICATE_KEYWORDS, is_sensitive_object,
        is_sensitive_predicate,
    };
    use crate::Database;

    #[test]
    fn sensitive_object_cleanup_matches_detector() {
        let db = Database::open_in_memory().unwrap();
        let mut cases: Vec<(String, bool)> = SENSITIVE_OBJECT_PREFIXES
            .iter()
            .map(|prefix| (format!("{prefix}secret"), true))
            .collect();
        cases.extend([
            ("\tghp_secret\t".into(), true),
            ("\u{00a0}npm_secret\u{00a0}".into(), true),
            ("eyJheader.payload.signature".into(), true),
            ("-----BEGIN PRIVATE KEY-----".into(), true),
            ("api_key=secret".into(), true),
            ("APIKEY=secret".into(), true),
            ("https://user:password@example.test/path".into(), true),
            ("ghpXordinary".into(), false),
            ("ghoXordinary".into(), false),
            ("ghsXordinary".into(), false),
            ("githubXpat_secret".into(), false),
            ("github_patXordinary".into(), false),
            ("npmXordinary".into(), false),
            ("dopXv1_secret".into(), false),
            ("dop_v1Xordinary".into(), false),
            ("https://docs.example/path".into(), false),
            (
                "person@example.test describes https://docs.example/path".into(),
                false,
            ),
            ("Rust programming language".into(), false),
        ]);

        for (index, (object, expected_sensitive)) in cases.iter().enumerate() {
            assert_eq!(
                is_sensitive_object(object),
                *expected_sensitive,
                "unexpected detector result for {object}"
            );
            db.insert_fact(
                "user",
                &format!("value_{index}"),
                object,
                "inferred",
                1.0,
                &[],
            )
            .unwrap();
        }

        let expected_remaining: Vec<_> = cases
            .iter()
            .filter(|(_, sensitive)| !sensitive)
            .map(|(object, _)| object.clone())
            .collect();
        let expected_deleted = (cases.len() - expected_remaining.len()) as u64;
        assert_eq!(db.delete_sensitive_facts().unwrap(), expected_deleted);

        let remaining: Vec<_> = db
            .list_facts_by_subject("user")
            .unwrap()
            .into_iter()
            .map(|fact| fact.object)
            .collect();
        assert_eq!(remaining.len(), expected_remaining.len());
        for object in expected_remaining {
            assert!(remaining.contains(&object), "cleanup removed {object}");
        }
    }

    #[test]
    fn sensitive_predicate_cleanup_matches_detector() {
        let db = Database::open_in_memory().unwrap();
        let mut cases: Vec<(String, bool)> = SENSITIVE_PREDICATE_KEYWORDS
            .iter()
            .map(|keyword| (format!("field_{keyword}_value"), true))
            .collect();
        cases.push(("preference".into(), false));

        for (predicate, expected_sensitive) in &cases {
            assert_eq!(
                is_sensitive_predicate(predicate),
                *expected_sensitive,
                "unexpected detector result for {predicate}"
            );
            db.insert_fact("user", predicate, "ordinary value", "inferred", 1.0, &[])
                .unwrap();
        }

        let deleted = db.delete_sensitive_facts().unwrap();
        assert_eq!(deleted, (cases.len() - 1) as u64);
        let remaining = db.list_facts().unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].predicate, "preference");
    }
}
