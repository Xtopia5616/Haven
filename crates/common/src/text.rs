//! Shared text helpers used across crates (prompt assembly, memory fields,
//! tool indexes). Kept in one place so the sanitization policy cannot drift.

use std::collections::HashSet;

/// English stopwords too generic to carry session-relevance signal when
/// scoring facts / episodes. Shared by prompt recall and `recall_memory`.
const MEMORY_TERM_STOPWORDS: &[&str] = &[
    "the", "and", "for", "with", "this", "that", "you", "your", "please", "are", "was", "were",
    "not", "but", "from", "have", "has", "all", "any", "can", "could", "would", "should", "will",
    "just", "about",
];

/// Hard cap so a long session description cannot flood FTS / scoring.
const MEMORY_TERM_MAX: usize = 24;

/// Cap trigrams emitted from one contiguous CJK run.
const MEMORY_CJK_NGRAMS_PER_RUN: usize = 8;

fn is_cjk(c: char) -> bool {
    matches!(
        c,
        '\u{3400}'..='\u{4DBF}' // CJK Ext A
            | '\u{4E00}'..='\u{9FFF}' // CJK Unified
            | '\u{F900}'..='\u{FAFF}' // CJK Compatibility
            | '\u{3040}'..='\u{30FF}' // Hiragana + Katakana
            | '\u{AC00}'..='\u{D7AF}' // Hangul syllables
    )
}

fn is_latin_term_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// Sanitize a string before interpolating it into an LLM prompt or storing it
/// as a memory field: replaces control characters (newlines, tabs) that could
/// be used for indirect prompt injection with a space, and caps the length.
/// Single shared implementation used by the agent prompt builder, fact
/// inference and tool index assembly.
pub fn sanitize_prompt_field(s: &str, max_chars: usize) -> String {
    s.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .take(max_chars)
        .collect()
}

/// Extract recall keywords from a session description or free-text query.
///
/// - Latin/digit tokens: lowercased, kept when ≥3 **characters** (not bytes)
///   and not an English stopword.
/// - Contiguous CJK runs: overlapping 3-grams (and the full digram when the
///   run is exactly 2 chars) so keyword FTS trigram / substring scoring works
///   without an embedding model. Pure alphanumeric splits leave Chinese as one
///   giant term and break recall.
pub fn memory_recall_terms(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();

    let mut push = |term: String| {
        if out.len() >= MEMORY_TERM_MAX {
            return;
        }
        if seen.insert(term.clone()) {
            out.push(term);
        }
    };

    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if is_cjk(c) {
            let start = i;
            i += 1;
            while i < chars.len() && is_cjk(chars[i]) {
                i += 1;
            }
            let run_chars = &chars[start..i];
            let n = run_chars.len();
            if n >= 3 {
                let total = n - 2;
                if total <= MEMORY_CJK_NGRAMS_PER_RUN {
                    for w in 0..total {
                        push(run_chars[w..w + 3].iter().collect());
                    }
                } else {
                    // Long contiguous CJK (typical session titles): keep head
                    // and tail windows so content near the end is not dropped.
                    let head = MEMORY_CJK_NGRAMS_PER_RUN / 2;
                    let tail = MEMORY_CJK_NGRAMS_PER_RUN - head;
                    for w in 0..head {
                        push(run_chars[w..w + 3].iter().collect());
                    }
                    let tail_start = total - tail;
                    for w in tail_start..total {
                        push(run_chars[w..w + 3].iter().collect());
                    }
                }
            } else if n == 2 {
                push(run_chars.iter().collect());
            }
        } else if is_latin_term_char(c) {
            let start = i;
            i += 1;
            while i < chars.len() && is_latin_term_char(chars[i]) {
                i += 1;
            }
            let raw: String = chars[start..i]
                .iter()
                .flat_map(|ch| ch.to_lowercase())
                .collect();
            if raw.chars().count() >= 3 && !MEMORY_TERM_STOPWORDS.contains(&raw.as_str()) {
                push(raw);
            }
        } else {
            i += 1;
        }
    }
    out
}

/// Pick up to `max` terms for FTS / keyword search: prefer head and tail of
/// the recall-term list so long CJK runs (whose n-grams are already
/// head+tail ordered) still search content near the end.
pub fn memory_recall_term_sample(terms: &[String], max: usize) -> Vec<&str> {
    if max == 0 || terms.is_empty() {
        return Vec::new();
    }
    if terms.len() <= max {
        return terms.iter().map(String::as_str).collect();
    }
    let head = max / 2;
    let tail = max - head;
    let mut out: Vec<&str> = Vec::with_capacity(max);
    let mut seen: HashSet<&str> = HashSet::new();
    for t in &terms[..head] {
        if seen.insert(t.as_str()) {
            out.push(t.as_str());
        }
    }
    for t in &terms[terms.len() - tail..] {
        if seen.insert(t.as_str()) {
            out.push(t.as_str());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replaces_control_chars_with_space() {
        let result = sanitize_prompt_field("hello\nworld\tIGNORE", 256);
        assert!(!result.contains('\n'));
        assert!(!result.contains('\t'));
        assert!(result.contains("hello"));
    }

    #[test]
    fn caps_length() {
        let result = sanitize_prompt_field(&"x".repeat(500), 256);
        assert_eq!(result.len(), 256);
    }

    #[test]
    fn preserves_normal_text() {
        assert_eq!(
            sanitize_prompt_field("Alice likes Rust", 256),
            "Alice likes Rust"
        );
    }

    #[test]
    fn recall_terms_latin_filters_stopwords_and_short() {
        let terms = memory_recall_terms("set up the dark theme for Rust");
        assert!(terms.contains(&"dark".into()));
        assert!(terms.contains(&"theme".into()));
        assert!(terms.contains(&"rust".into()));
        assert!(!terms.iter().any(|t| t == "the" || t == "for" || t == "up"));
    }

    #[test]
    fn recall_terms_cjk_emits_trigrams() {
        let terms = memory_recall_terms("帮我配置喝咖啡提醒");
        assert!(
            terms.iter().any(|t| t == "喝咖啡"),
            "expected 喝咖啡 trigram, got {:?}",
            terms
        );
        assert!(
            terms.iter().any(|t| t == "配置喝" || t == "我配置" || t.contains('配')),
            "expected surrounding CJK trigrams, got {:?}",
            terms
        );
    }

    #[test]
    fn recall_terms_mixed_script() {
        let terms = memory_recall_terms("Haven 项目的 dark theme");
        assert!(terms.contains(&"haven".into()));
        assert!(terms.contains(&"dark".into()));
        assert!(terms.contains(&"theme".into()));
        assert!(
            terms.iter().any(|t| t.contains('项') || t.contains('目')),
            "expected CJK ngrams from 项目的, got {:?}",
            terms
        );
    }

    #[test]
    fn recall_terms_uses_char_len_not_bytes() {
        // Two CJK chars = 6 bytes but only a digram term, not dropped as "short".
        let terms = memory_recall_terms("主题");
        assert_eq!(terms, vec!["主题".to_string()]);
    }

    #[test]
    fn recall_terms_long_cjk_keeps_tail_trigrams() {
        // 20+ chars: first-8-only would miss 喝咖啡 near the end.
        let text = "请帮我设置一个每天早上七点提醒我喝咖啡好吗";
        let terms = memory_recall_terms(text);
        assert!(
            terms.iter().any(|t| t == "喝咖啡"),
            "long CJK run must keep tail trigram 喝咖啡, got {:?}",
            terms
        );
        assert!(
            terms.iter().any(|t| t.starts_with('请') || t.contains("帮我")),
            "long CJK run must also keep head trigrams, got {:?}",
            terms
        );
        let sample = memory_recall_term_sample(&terms, 6);
        assert!(
            sample.iter().any(|t| *t == "喝咖啡"),
            "search sample of 6 must still include tail 喝咖啡, got {:?}",
            sample
        );
    }
}
