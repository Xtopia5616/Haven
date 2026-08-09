//! Shared text helpers used across crates (prompt assembly, memory fields,
//! tool indexes). Kept in one place so the sanitization policy cannot drift.

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
}
