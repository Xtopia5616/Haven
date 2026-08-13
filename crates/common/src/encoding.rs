/// Decode bytes to a String, handling Windows console code pages.
///
/// First tries UTF-8 (the common case for most tools/configs), stripping a
/// UTF-8 BOM (Windows PowerShell 5.1 `Out-File -Encoding utf8` writes one).
/// If the bytes are not valid UTF-8, UTF-16 output — produced by PowerShell
/// redirection (`>`) on 5.1, which writes UTF-16LE with a BOM, and by tools
/// emitting UTF-16 without one — is decoded before falling back to GBK
/// (cmd.exe output on Chinese Windows using CP936).
pub fn decode_lossy(bytes: &[u8]) -> String {
    if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
        return match std::str::from_utf8(&bytes[3..]) {
            Ok(s) => s.to_string(),
            Err(_) => decode_utf16_or_gbk(&bytes[3..]),
        };
    }
    // UTF-16 BOMs must be consumed before the no-BOM heuristic, or the BOM
    // bytes of ASCII-heavy payloads would be decoded as a leading U+FEFF.
    if bytes.starts_with(&[0xFF, 0xFE]) {
        return decode_utf16le(&bytes[2..]);
    }
    if bytes.starts_with(&[0xFE, 0xFF]) {
        return decode_utf16be(&bytes[2..]);
    }
    // UTF-16LE text without a BOM is usually still valid UTF-8 (ASCII code
    // units are < 0x80), so the heuristic must run before the UTF-8 check
    // or `e\0c\0h\0o…` would pass as text.
    if looks_like_utf16le(bytes) {
        return decode_utf16le(bytes);
    }
    if let Ok(s) = std::str::from_utf8(bytes) {
        return s.to_string();
    }
    decode_utf16_or_gbk(bytes)
}

/// Decode bytes for display when the buffer may be cut mid-sequence.
///
/// Valid UTF-8 is returned as-is. If the buffer ends with an incomplete UTF-8
/// sequence (at most 3 trailing bytes), that tail is dropped so a UTF-8 file
/// preview never triggers the fallbacks below (which would garble the whole
/// valid prefix). Otherwise the bytes are decoded as UTF-16 (BOM-detected, or
/// a no-BOM UTF-16LE heuristic) or, failing that, as GBK (Windows console
/// code pages, e.g. CP936).
pub fn decode_preview(bytes: &[u8]) -> String {
    if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
        return std::str::from_utf8(&bytes[3..])
            .map(|s| s.to_string())
            .unwrap_or_else(|_| decode_utf16_or_gbk(&bytes[3..]));
    }
    if bytes.starts_with(&[0xFF, 0xFE]) || bytes.starts_with(&[0xFE, 0xFF]) {
        return decode_utf16_or_gbk(bytes);
    }
    // Same reasoning as decode_lossy: BOM-less UTF-16LE can look like UTF-8.
    if looks_like_utf16le(bytes) {
        return decode_utf16le(bytes);
    }
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

/// Decode non-UTF-8 bytes: UTF-16 when a BOM or the no-BOM heuristic matches,
/// otherwise GBK lossy (Windows console code pages).
fn decode_utf16_or_gbk(bytes: &[u8]) -> String {
    if bytes.starts_with(&[0xFF, 0xFE]) {
        return decode_utf16le(&bytes[2..]);
    }
    if bytes.starts_with(&[0xFE, 0xFF]) {
        return decode_utf16be(&bytes[2..]);
    }
    if looks_like_utf16le(bytes) {
        return decode_utf16le(bytes);
    }
    let (cow, _encoding, _had_errors) = encoding_rs::GBK.decode(bytes);
    cow.to_string()
}

fn decode_utf16le(bytes: &[u8]) -> String {
    let units: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect();
    String::from_utf16_lossy(&units)
}

fn decode_utf16be(bytes: &[u8]) -> String {
    let units: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|c| u16::from_be_bytes([c[0], c[1]]))
        .collect();
    String::from_utf16_lossy(&units)
}

/// Conservative no-BOM UTF-16LE heuristic: even-length input where most 16-bit
/// units land in printable ranges (ASCII, CJK, CJK punctuation/fullwidth) and
/// NUL-high-byte (ASCII) units are a strict majority. Genuine BOM-less
/// UTF-16LE text has NULs on ~every other byte; GBK bytes, plain ASCII/CJK
/// UTF-8 and NUL-separated string tables fail one of the two guards.
fn looks_like_utf16le(bytes: &[u8]) -> bool {
    if bytes.len() < 4 || !bytes.len().is_multiple_of(2) {
        return false;
    }
    // Any accepted input must contain NUL high bytes, and plain ASCII/CJK
    // UTF-8 never does — reject it cheaply before the scoring loop (this is
    // the hot path for every valid-UTF-8 decode).
    if !bytes.contains(&0) {
        return false;
    }
    let pairs = bytes.len() / 2;
    let mut printable = 0usize;
    let mut ascii = 0usize;
    for i in 0..pairs {
        let lo = bytes[2 * i] as u16;
        let hi = bytes[2 * i + 1] as u16;
        let unit = lo | (hi << 8);
        if unit == 0
            || unit == 9
            || unit == 10
            || unit == 13
            || unit == 32
            || (0x21..=0x7E).contains(&unit)
            || (0x3000..=0x9FFF).contains(&unit)
            || (0xFF00..=0xFFEF).contains(&unit)
        {
            printable += 1;
            if unit == 32 || (0x21..=0x7E).contains(&unit) {
                ascii += 1;
            }
        }
    }
    // Strict majority: NUL-separated ASCII string tables (e.g. `abc\0def\0`)
    // sit at exactly 50% ASCII units and must not be taken for UTF-16LE.
    printable * 10 >= pairs * 8 && ascii * 2 > pairs
}

/// Decode XML entities. `&amp;` is unescaped last so a literal `&amp;lt;`
/// becomes `&lt;` (text), not a double-decoded `<`. Shared by the CLIXML
/// message decoding (haven-tools) and the scheduled-task XML parsing
/// (haven-app-binary) so the two can never diverge.
pub fn xml_unescape(text: &str) -> String {
    text.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
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
    fn decode_lossy_utf8_bom_stripped() {
        // Windows PowerShell 5.1 `Out-File -Encoding utf8` writes a BOM.
        let mut bytes = vec![0xEF, 0xBB, 0xBF];
        bytes.extend_from_slice("你好".as_bytes());
        assert_eq!(decode_lossy(&bytes), "你好");
        assert_eq!(decode_preview(&bytes), "你好");
    }

    #[test]
    fn decode_lossy_utf16le_bom() {
        // Windows PowerShell 5.1 `>` redirection writes UTF-16LE with BOM.
        let mut bytes = vec![0xFF, 0xFE];
        for unit in "你好".encode_utf16() {
            bytes.extend_from_slice(&unit.to_le_bytes());
        }
        assert_eq!(decode_lossy(&bytes), "你好");
        assert_eq!(decode_preview(&bytes), "你好");
    }

    #[test]
    fn decode_lossy_utf16be_bom() {
        let mut bytes = vec![0xFE, 0xFF];
        for unit in "hello 你好".encode_utf16() {
            bytes.extend_from_slice(&unit.to_be_bytes());
        }
        assert_eq!(decode_lossy(&bytes), "hello 你好");
    }

    #[test]
    fn decode_lossy_utf16le_without_bom() {
        // `[Console]::OutputEncoding = [Text.Encoding]::Unicode` pipes UTF-16LE
        // with no BOM; the heuristic must still pick it up.
        let mut bytes = Vec::new();
        for unit in "echo: connection refused".encode_utf16() {
            bytes.extend_from_slice(&unit.to_le_bytes());
        }
        assert_eq!(decode_lossy(&bytes), "echo: connection refused");
        assert_eq!(decode_preview(&bytes), "echo: connection refused");
    }

    #[test]
    fn decode_lossy_gbk_not_mistaken_for_utf16() {
        // "你好" in GBK (CP936) must stay on the GBK path, not be misdetected
        // as UTF-16LE by the no-BOM heuristic.
        let gbk_bytes = [0xC4, 0xE3, 0xBA, 0xC3];
        assert_eq!(decode_lossy(&gbk_bytes), "你好");
        assert!(!looks_like_utf16le(&gbk_bytes));
    }

    #[test]
    fn decode_lossy_ascii_not_mistaken_for_utf16() {
        let text = "hello world from a legacy gbk tool";
        assert_eq!(decode_lossy(text.as_bytes()), text);
        assert!(!looks_like_utf16le(text.as_bytes()));
    }

    #[test]
    fn decode_lossy_utf16le_bom_ascii_has_no_bom_leak() {
        // ASCII-heavy BOM'd UTF-16LE must not carry a leading U+FEFF (the
        // heuristic previously fired before the BOM check and decoded the BOM).
        let mut bytes = vec![0xFF, 0xFE];
        for unit in "ABCDEFGHIJ".encode_utf16() {
            bytes.extend_from_slice(&unit.to_le_bytes());
        }
        let s = decode_lossy(&bytes);
        assert_eq!(s, "ABCDEFGHIJ", "got: {s:?}");
        assert!(!s.contains('\u{FEFF}'), "BOM must not leak: {s:?}");
        assert_eq!(
            decode_preview(&bytes),
            s,
            "decode_lossy and preview must agree"
        );
    }

    #[test]
    fn decode_lossy_nul_separated_utf8_not_mistaken_for_utf16() {
        // Valid UTF-8 with embedded NULs (string tables, REG_MULTI_SZ) must
        // decode as itself, not be garbled as UTF-16LE.
        let bytes = b"abc\0def\0ghi\0jkl\0";
        assert_eq!(
            decode_lossy(bytes),
            String::from_utf8(bytes.to_vec()).unwrap()
        );
        assert_eq!(
            decode_preview(bytes),
            String::from_utf8(bytes.to_vec()).unwrap()
        );
        assert!(!looks_like_utf16le(bytes));
    }

    #[test]
    fn xml_unescape_handles_nested_entities_once() {
        assert_eq!(
            xml_unescape("a&amp;b&lt;c&gt;d&quot;e&apos;f"),
            "a&b<c>d\"e'f"
        );
        // A literal `&amp;lt;` means the text `&lt;` — unescaping &amp; last
        // must not double-decode it into `<`.
        assert_eq!(xml_unescape("&amp;lt;"), "&lt;");
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
