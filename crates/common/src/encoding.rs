/// Decode bytes to a String, handling Windows console code pages.
///
/// First tries UTF-8 (the common case for most tools/configs).
/// If the bytes are not valid UTF-8 (e.g. cmd.exe output on Chinese Windows
/// using CP936/GBK), falls back to GBK decoding as a lossy best-effort.
pub fn decode_lossy(bytes: &[u8]) -> String {
    if let Ok(s) = std::str::from_utf8(bytes) {
        return s.to_string();
    }
    let (cow, _encoding, _had_errors) = encoding_rs::GBK.decode(bytes);
    cow.to_string()
}

/// Truncate `text` to at most `max_chars` bytes (char-boundary safe), appending
/// an "omitted" marker when truncation happened. Returns `(output, truncated)`.
pub fn truncate_output(text: &str, max_chars: usize) -> (String, bool) {
    if text.len() <= max_chars {
        (text.to_string(), false)
    } else {
        let cutoff = text.floor_char_boundary(max_chars);
        let truncated = format!(
            "{}[truncated ... {} chars omitted]",
            &text[..cutoff],
            text.len() - cutoff
        );
        (truncated, true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_valid_utf8() {
        assert_eq!(decode_lossy(b"hello world"), "hello world");
    }

    #[test]
    fn decode_empty_bytes() {
        assert_eq!(decode_lossy(b""), "");
    }

    #[test]
    fn decode_multibyte_utf8() {
        let s = "你好世界";
        assert_eq!(decode_lossy(s.as_bytes()), s);
    }

    #[test]
    fn decode_invalid_utf8_falls_back_to_gbk() {
        // "你好" encoded in GBK (CP936): 0xC4 0xE3 0xBA 0xC3
        let gbk_bytes = [0xC4, 0xE3, 0xBA, 0xC3];
        let decoded = decode_lossy(&gbk_bytes);
        assert_eq!(decoded, "你好");
    }

    #[test]
    fn decode_mixed_utf8_ascii() {
        let s = "rust says: \u{4f60}\u{597d}";
        assert_eq!(decode_lossy(s.as_bytes()), s);
    }

    #[test]
    fn truncate_output_short() {
        let (out, truncated) = truncate_output("hello", 100);
        assert_eq!(out, "hello");
        assert!(!truncated);
    }

    #[test]
    fn truncate_output_long() {
        let text = "a".repeat(200);
        let (out, truncated) = truncate_output(&text, 100);
        assert!(truncated);
        assert!(out.len() < text.len());
        assert!(out.contains("[truncated ... 100 chars omitted]"));
    }

    #[test]
    fn truncate_output_multibyte_boundary() {
        let text = "中文内容".repeat(50);
        let (out, truncated) = truncate_output(&text, 30);
        assert!(truncated);
        assert!(out.is_char_boundary(out.len()));
    }
}
