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
