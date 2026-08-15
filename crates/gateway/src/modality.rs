//! Stage 1: modality detection — zero-cost, rule-first.
//!
//! Detection order: magic bytes (real content type, filename-independent) →
//! filename extension fallback → text-decoding fallback (UTF-8 then GBK,
//! which covers Chinese text files on Windows) → `Unknown`.

/// Detected input modality. Maps directly to the gateway's routing key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Modality {
    Text,
    Image,
    Audio,
    Video,
    Document,
    Unknown,
}

impl Modality {
    pub const fn as_str(self) -> &'static str {
        match self {
            Modality::Text => "text",
            Modality::Image => "image",
            Modality::Audio => "audio",
            Modality::Video => "video",
            Modality::Document => "document",
            Modality::Unknown => "unknown",
        }
    }
}

/// Detect the modality of raw bytes, using `filename` only as a fallback
/// when magic bytes are inconclusive.
pub fn detect_modality(data: &[u8], filename: &str) -> Modality {
    if let Some(m) = detect_magic(data) {
        return m;
    }
    if let Some(m) = detect_extension(filename) {
        return m;
    }
    if looks_like_text(data) {
        return Modality::Text;
    }
    Modality::Unknown
}

/// Magic-byte detection table (magic constants for common media types).
fn detect_magic(data: &[u8]) -> Option<Modality> {
    // Images
    if data.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return Some(Modality::Image); // JPEG
    }
    if data.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) {
        return Some(Modality::Image); // PNG
    }
    if data.starts_with(b"GIF87a") || data.starts_with(b"GIF89a") {
        return Some(Modality::Image); // GIF
    }
    if data.starts_with(b"BM") {
        return Some(Modality::Image); // BMP
    }
    if data.starts_with(&[0x49, 0x49, 0x2A, 0x00]) || data.starts_with(&[0x4D, 0x4D, 0x00, 0x2A]) {
        return Some(Modality::Image); // TIFF (little/big endian)
    }
    if data.starts_with(&[0x00, 0x00, 0x01, 0x00]) {
        return Some(Modality::Image); // ICO
    }
    if data.len() >= 12 && data.starts_with(b"RIFF") && &data[8..12] == b"WEBP" {
        return Some(Modality::Image); // WEBP
    }

    // Audio
    if data.starts_with(b"ID3") {
        return Some(Modality::Audio); // MP3 with ID3 tag
    }
    if data.len() >= 2 && data[0] == 0xFF && (data[1] & 0xE0) == 0xE0 && (data[1] & 0x18) != 0x08 {
        return Some(Modality::Audio); // MPEG audio frame sync
    }
    if data.len() >= 12 && data.starts_with(b"RIFF") && &data[8..12] == b"WAVE" {
        return Some(Modality::Audio); // WAV
    }
    if data.starts_with(b"fLaC") {
        return Some(Modality::Audio); // FLAC
    }
    if data.starts_with(b"OggS") {
        return Some(Modality::Audio); // OGG (audio or video container)
    }

    // Video
    if data.len() >= 12 && data.starts_with(b"RIFF") && &data[8..12] == b"AVI " {
        return Some(Modality::Video); // AVI
    }
    if data.starts_with(&[0x1A, 0x45, 0xDF, 0xA3]) {
        // EBML container: Matroska/WebM — video container regardless of
        // audio-only content (safe default).
        return Some(Modality::Video);
    }
    if data.starts_with(&[0x30, 0x26, 0xB2, 0x75, 0x8E, 0x66, 0xCF, 0x11]) {
        return Some(Modality::Video); // ASF/WMV
    }
    if data.len() >= 12 && &data[4..8] == b"ftyp" {
        // ISO-BMFF (MP4/M4A/MOV). Audio-only brands map to Audio, everything
        // else to Video.
        let brand = &data[8..12];
        if brand == b"M4A " || brand == b"f4a " || brand == b"M4B " || brand == b"M4P " {
            return Some(Modality::Audio);
        }
        return Some(Modality::Video);
    }

    // Documents
    if data.starts_with(b"%PDF") {
        return Some(Modality::Document); // PDF
    }
    if data.starts_with(&[0x50, 0x4B, 0x03, 0x04]) {
        // ZIP container: docx/xlsx/pptx. Extension decides below (the ZIP
        // magic alone cannot distinguish archives from Office documents), so
        // only fall through to the extension check.
        return None;
    }

    None
}

/// Filename extension fallback for magic bytes that are missing or ambiguous.
fn detect_extension(filename: &str) -> Option<Modality> {
    let name = filename.to_ascii_lowercase();
    let ext = name.rsplit('.').next()?;
    match ext {
        "jpg" | "jpeg" | "png" | "gif" | "webp" | "bmp" | "tiff" | "tif" | "ico" | "svg"
        | "heic" | "avif" => Some(Modality::Image),
        "mp3" | "wav" | "flac" | "ogg" | "oga" | "m4a" | "aac" | "wma" | "opus" | "amr" => {
            Some(Modality::Audio)
        }
        "mp4" | "webm" | "mkv" | "avi" | "mov" | "wmv" | "flv" | "m4v" | "ts" | "mts" | "3gp" => {
            Some(Modality::Video)
        }
        "pdf" => Some(Modality::Document),
        // Text-like extensions (`ts` is deliberately absent: it is the
        // MPEG-TS video stream extension, and TypeScript files decode as
        // text via the fallback below).
        "txt" | "md" | "markdown" | "json" | "csv" | "log" | "xml" | "html" | "htm" | "yaml"
        | "yml" | "toml" | "ini" | "conf" | "cfg" | "py" | "rs" | "js" | "css" | "svelte"
        | "sql" | "bat" | "ps1" | "sh" | "c" | "h" | "cpp" | "hpp" | "java" | "go" | "rb"
        | "php" | "lua" | "r" | "tsv" | "vtt" | "srt" => Some(Modality::Text),
        // Office documents (ZIP containers are ambiguous by magic alone).
        "docx" | "xlsx" | "pptx" | "doc" | "xls" | "ppt" | "odt" | "ods" | "rtf" | "epub" => {
            Some(Modality::Document)
        }
        _ => None,
    }
}

/// Whether the bytes look like readable text: strict UTF-8, or GBK/CP936
/// (common on Chinese Windows) without decode errors — plus a control-char
/// sanity check that rejects ASCII-binary that happens to be valid UTF-8
/// (ZIP headers, BMP/ICO headers, …).
fn looks_like_text(data: &[u8]) -> bool {
    if data.is_empty() {
        return true;
    }
    if control_ratio(data) > 0.02 {
        return false;
    }
    if std::str::from_utf8(data).is_ok() {
        return true;
    }
    let (_, _, had_errors) = encoding_rs::GBK.decode(data);
    !had_errors
}

/// Fraction of ASCII control bytes (excluding \n \r \t) — a fingerprint of
/// binary headers. Text files of any encoding stay near zero.
fn control_ratio(data: &[u8]) -> f32 {
    if data.is_empty() {
        return 0.0;
    }
    let controls = data
        .iter()
        .filter(|b| {
            let b = **b;
            b < 0x09 || (b > 0x0D && b < 0x20)
        })
        .count();
    controls as f32 / data.len() as f32
}

/// Guess the MIME type from magic bytes (used when building content parts
/// for the main model). Falls back to `application/octet-stream`.
pub fn detect_media_type(data: &[u8]) -> &'static str {
    if data.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return "image/jpeg";
    }
    if data.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) {
        return "image/png";
    }
    if data.starts_with(b"GIF87a") || data.starts_with(b"GIF89a") {
        return "image/gif";
    }
    if data.starts_with(b"RIFF") {
        if data.len() >= 12 && &data[8..12] == b"WEBP" {
            return "image/webp";
        }
        if data.len() >= 12 && &data[8..12] == b"WAVE" {
            return "audio/wav";
        }
        if data.len() >= 12 && &data[8..12] == b"AVI " {
            return "video/x-msvideo";
        }
    }
    if data.starts_with(b"ID3") {
        return "audio/mpeg";
    }
    if data.len() >= 2 && data[0] == 0xFF && (data[1] & 0xE0) == 0xE0 && (data[1] & 0x18) != 0x08 {
        return "audio/mpeg";
    }
    if data.starts_with(b"fLaC") {
        return "audio/flac";
    }
    if data.starts_with(b"OggS") {
        return "audio/ogg";
    }
    if data.starts_with(b"%PDF") {
        return "application/pdf";
    }
    if data.len() >= 12 && &data[4..8] == b"ftyp" {
        let brand = &data[8..12];
        if brand == b"M4A " || brand == b"f4a " || brand == b"M4B " || brand == b"M4P " {
            return "audio/mp4";
        }
        return "video/mp4";
    }
    if data.starts_with(&[0x1A, 0x45, 0xDF, 0xA3]) {
        return "video/webm";
    }
    if data.starts_with(&[0x49, 0x49, 0x2A, 0x00]) || data.starts_with(&[0x4D, 0x4D, 0x00, 0x2A]) {
        return "image/tiff";
    }
    if data.starts_with(b"BM") {
        return "image/bmp";
    }
    "application/octet-stream"
}

/// Derive a file extension from a MIME type (for generated media files).
pub fn extension_for_media_type(media_type: &str) -> &'static str {
    match media_type {
        "image/png" => "png",
        "image/jpeg" | "image/jpg" => "jpg",
        "image/webp" => "webp",
        "image/gif" => "gif",
        "audio/mpeg" => "mp3",
        "audio/wav" => "wav",
        "audio/mp4" => "m4a",
        "audio/ogg" => "ogg",
        "audio/flac" => "flac",
        "video/mp4" => "mp4",
        "video/webm" => "webm",
        _ => "bin",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn detect(data: &[u8]) -> Modality {
        detect_modality(data, "")
    }

    #[test]
    fn image_magics() {
        assert_eq!(detect(&[0xFF, 0xD8, 0xFF, 0xE0]), Modality::Image);
        assert_eq!(detect(b"\x89PNG\r\n\x1a\n...."), Modality::Image);
        assert_eq!(detect(b"GIF89a...."), Modality::Image);
        assert_eq!(detect(b"BM...."), Modality::Image);
        assert_eq!(detect(&[0x49, 0x49, 0x2A, 0x00, 0x08]), Modality::Image);
        assert_eq!(detect(b"RIFF....WEBP"), Modality::Image);
        assert_eq!(detect(&[0x00, 0x00, 0x01, 0x00, 0x01]), Modality::Image);
    }

    #[test]
    fn audio_magics() {
        assert_eq!(detect(b"ID3\x04\x00...."), Modality::Audio);
        assert_eq!(detect(&[0xFF, 0xFB, 0x90, 0x00]), Modality::Audio);
        assert_eq!(detect(b"RIFF....WAVE"), Modality::Audio);
        assert_eq!(detect(b"fLaC\x00\x00\x00\x22"), Modality::Audio);
        assert_eq!(detect(b"OggS\x00\x02...."), Modality::Audio);
        // ISO-BMFF audio brand
        let mut m4a = b"\x00\x00\x00\x18ftypM4A \x00\x00\x00\x00".to_vec();
        m4a.resize(32, 0);
        assert_eq!(detect(&m4a), Modality::Audio);
    }

    #[test]
    fn video_magics() {
        assert_eq!(detect(b"RIFF....AVI "), Modality::Video);
        assert_eq!(detect(&[0x1A, 0x45, 0xDF, 0xA3, 0x01]), Modality::Video);
        assert_eq!(
            detect(&[0x30, 0x26, 0xB2, 0x75, 0x8E, 0x66, 0xCF, 0x11]),
            Modality::Video
        );
        let mut mp4 = b"\x00\x00\x00\x18ftypisom\x00\x00\x00\x00".to_vec();
        mp4.resize(32, 0);
        assert_eq!(detect(&mp4), Modality::Video);
    }

    #[test]
    fn document_magics() {
        assert_eq!(detect(b"%PDF-1.7\n..."), Modality::Document);
        // ZIP container alone is ambiguous → extension decides.
        let zip = [0x50, 0x4B, 0x03, 0x04, 0x14, 0x00, 0x00, 0x00];
        assert_eq!(detect_modality(&zip, ""), Modality::Unknown);
        assert_eq!(detect_modality(&zip, "report.docx"), Modality::Document);
        assert_eq!(detect_modality(&zip, "archive.zip"), Modality::Unknown);
    }

    #[test]
    fn text_fallbacks() {
        assert_eq!(detect(b"hello world\nsecond line"), Modality::Text);
        // UTF-8 Chinese text
        assert_eq!(detect("你好，世界".as_bytes()), Modality::Text);
        // GBK-encoded Chinese text (invalid UTF-8, valid GBK)
        let (encoded, _, had_errors) = encoding_rs::GBK.encode("中文文本内容");
        assert!(!had_errors);
        assert_eq!(detect(&encoded), Modality::Text);
    }

    #[test]
    fn extension_fallbacks() {
        assert_eq!(
            detect_modality(b"\x00\x01\x02", "photo.JPG"),
            Modality::Image
        );
        assert_eq!(
            detect_modality(b"\x00\x01\x02", "song.mp3"),
            Modality::Audio
        );
        assert_eq!(
            detect_modality(b"\x00\x01\x02", "clip.mkv"),
            Modality::Video
        );
        assert_eq!(
            detect_modality(b"\x00\x01\x02", "doc.pdf"),
            Modality::Document
        );
        assert_eq!(detect_modality(b"\x00\x01\x02", "notes.md"), Modality::Text);
    }

    #[test]
    fn binary_without_magic_and_extension_is_unknown() {
        assert_eq!(detect(&[0x00, 0x01, 0x02, 0xFF, 0xFE]), Modality::Unknown);
    }

    #[test]
    fn empty_data_is_text() {
        assert_eq!(detect(b""), Modality::Text);
    }
}
