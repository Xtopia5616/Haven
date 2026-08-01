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

/// Decode bytes for display when the buffer may be cut mid-sequence.
///
/// Valid UTF-8 is returned as-is. If the buffer ends with an incomplete UTF-8
/// sequence (at most 3 trailing bytes), that tail is dropped so a UTF-8 file
/// preview never triggers the GBK fallback below (which would garble the whole
/// valid prefix). Otherwise the bytes are decoded as GBK, lossy (Windows
/// console code pages, e.g. CP936).
pub fn decode_preview(bytes: &[u8]) -> String {
    match std::str::from_utf8(bytes) {
        Ok(s) => s.to_string(),
        Err(e) if bytes.len() - e.valid_up_to() <= 3 => {
            // Trailing partial UTF-8 sequence: keep only the valid prefix.
            std::str::from_utf8(&bytes[..e.valid_up_to()])
                .expect("valid_up_to() is a UTF-8 boundary")
                .to_string()
        }
        Err(_) => encoding_rs::GBK.decode(bytes).0.into_owned(),
    }
}

/// Truncate `text` to at most `max_chars` bytes (char-boundary safe), appending
/// an "omitted" marker when truncation happened. Returns `(output, truncated)`.
/// The marker counts omitted *chars* (not bytes) to match its wording.
pub fn truncate_output(text: &str, max_chars: usize) -> (String, bool) {
    if text.len() <= max_chars {
        (text.to_string(), false)
    } else {
        let cutoff = text.floor_char_boundary(max_chars);
        let omitted_chars = text[cutoff..].chars().count();
        let truncated = format!(
            "{}[truncated ... {} chars omitted]",
            &text[..cutoff],
            omitted_chars
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
    fn decode_preview_utf8() {
        assert_eq!(decode_preview(b"hello world"), "hello world");
        assert_eq!(decode_preview("你好世界".as_bytes()), "你好世界");
    }

    #[test]
    fn decode_preview_trailing_partial_sequence_dropped() {
        // 2 bytes of a 3-byte UTF-8 sequence at the tail: the valid prefix
        // must decode as UTF-8 instead of triggering the GBK fallback.
        let mut bytes = "中文".as_bytes().to_vec();
        bytes.extend_from_slice(&[0xE4, 0xB8]);
        assert_eq!(decode_preview(&bytes), "中文");
    }

    #[test]
    fn decode_preview_gbk_fallback() {
        let gbk_bytes = [0xC4, 0xE3, 0xBA, 0xC3];
        assert_eq!(decode_preview(&gbk_bytes), "你好");
    }

    #[test]
    fn decode_preview_empty() {
        assert_eq!(decode_preview(b""), "");
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

    #[test]
    fn truncate_output_marker_counts_chars_not_bytes() {
        // "中" is 3 bytes; 100 bytes = 33 chars, so 200 - 33 = 167 chars are
        // omitted (byte-counting would wrongly report 501).
        let text = "中".repeat(200);
        let (out, truncated) = truncate_output(&text, 100);
        assert!(truncated);
        assert!(
            out.contains("[truncated ... 167 chars omitted]"),
            "got: {}",
            out
        );
    }
}
