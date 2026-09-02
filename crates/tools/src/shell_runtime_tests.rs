use super::*;

// ── encode_utf16le_base64 ────────────────────────────────────────────

#[cfg(windows)]
#[test]
fn test_encode_utf16le_base64_roundtrips_unicode() {
    let script = "Write-Output \"中文 & $('quote')\"; %foo%";
    let encoded = encode_utf16le_base64(script);
    // The payload must be pure ASCII so no code page / escaping can touch it.
    assert!(encoded.is_ascii(), "payload must be ASCII: {encoded}");
    // Decode back: each char is a UTF-16LE unit, then UTF-16 -> String.
    use base64::Engine;
    let raw = base64::engine::general_purpose::STANDARD
        .decode(&encoded)
        .expect("valid base64");
    let units: Vec<u16> = raw
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect();
    assert_eq!(
        String::from_utf16(&units).expect("valid UTF-16"),
        script,
        "encoded command must round-trip exactly"
    );
}

#[cfg(windows)]
#[test]
fn test_build_shell_command_powershell_uses_encoded_command() {
    let cmd = build_shell_command_silent("powershell", "Write-Output hi");
    let args: Vec<_> = cmd
        .get_args()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    assert_eq!(args[0], "-NoProfile");
    assert_eq!(args[2], "-EncodedCommand");
    // The 4th arg is base64 UTF-16LE of the UTF-8-forced script.
    assert!(
        args[3]
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '+' || c == '/' || c == '=')
    );
    // The payload must pin Out-File to UTF-8 so `>`/Out-File redirection
    // on PS 5.1 never writes UTF-16 files the agent would garble later.
    use base64::Engine;
    let raw = base64::engine::general_purpose::STANDARD
        .decode(&args[3])
        .expect("valid base64");
    let units: Vec<u16> = raw
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect();
    let payload = String::from_utf16(&units).expect("valid UTF-16");
    assert!(
        payload.contains("$PSDefaultParameterValues['Out-File:Encoding'] = 'utf8'"),
        "redirection must default to UTF-8, got: {payload}"
    );
    assert!(
        payload.contains("$ProgressPreference = 'SilentlyContinue'"),
        "progress must be silenced so CLIXML ProgressRecords never hit stderr, got: {payload}"
    );
    assert!(payload.ends_with("Write-Output hi"));
}
