use super::*;

#[test]
fn tool_run_log_cleanup_removes_only_orphaned_canonical_logs() {
    let root = tempfile::tempdir().unwrap();
    let retained_id = "toolrun-11111111111111111111111111111111";
    let orphan_id = "toolrun-22222222222222222222222222222222";
    let deleted_id = "toolrun-33333333333333333333333333333333";
    let retained_path = root.path().join(format!("{retained_id}.log"));
    let orphan_path = root.path().join(format!("{orphan_id}.log"));
    let deleted_path = root.path().join(format!("{deleted_id}.log"));
    let unrelated_path = root.path().join("notes.log");
    std::fs::write(&retained_path, "still referenced").unwrap();
    std::fs::write(&orphan_path, "orphaned output").unwrap();
    std::fs::write(&deleted_path, "deleted history").unwrap();
    std::fs::write(&unrelated_path, "not a ToolRun log").unwrap();

    assert!(remove_tool_run_output_log_at(root.path(), deleted_id).unwrap());
    assert!(!deleted_path.exists());
    let retained = std::collections::HashSet::from([retained_id.to_string()]);
    assert_eq!(
        remove_orphaned_tool_run_output_logs_at(root.path(), &retained).unwrap(),
        1
    );
    assert!(retained_path.exists());
    assert!(!orphan_path.exists());
    assert!(unrelated_path.exists());
    assert!(!remove_tool_run_output_log_at(root.path(), "toolrun-../notes").unwrap());
}

#[test]
fn shell_log_cleanup_expires_only_owned_logs_and_old_atomic_staging() {
    let root = tempfile::tempdir().unwrap();
    let log_name = "shell-11111111111111111111111111111111.log";
    let staging_name = ".file-22222222222222222222222222222222.tmp";
    let unrelated_name = "notes.log";
    let invalid_shell_name = "shell-not-a-uuid.log";
    let legacy_name = "shell-1728000000.log";
    for name in [
        log_name,
        staging_name,
        unrelated_name,
        invalid_shell_name,
        legacy_name,
    ] {
        std::fs::write(root.path().join(name), "contents").unwrap();
    }

    let now = std::time::SystemTime::now() + SHELL_OUTPUT_LOG_RETENTION + Duration::from_secs(1);
    assert_eq!(
        cleanup_shell_output_logs_at(root.path(), now, None).unwrap(),
        3
    );
    assert!(!root.path().join(log_name).exists());
    assert!(!root.path().join(staging_name).exists());
    assert!(!root.path().join(legacy_name).exists());
    assert!(root.path().join(unrelated_name).exists());
    assert!(root.path().join(invalid_shell_name).exists());
}

#[test]
fn shell_log_budget_evicts_oldest_logs_and_reserves_new_entry() {
    let root = tempfile::tempdir().unwrap();
    for index in 0..=SHELL_OUTPUT_LOG_MAX_COUNT {
        let name = format!("shell-{index:032x}.log");
        std::fs::write(root.path().join(name), "x").unwrap();
    }

    assert_eq!(
        cleanup_shell_output_logs_at(root.path(), std::time::SystemTime::now(), Some(1)).unwrap(),
        2
    );
    let retained_count = std::fs::read_dir(root.path())
        .unwrap()
        .filter_map(Result::ok)
        .filter(|entry| is_shell_output_log_name(&entry.file_name().to_string_lossy()))
        .count();
    assert_eq!(retained_count, SHELL_OUTPUT_LOG_MAX_COUNT - 1);
}

#[test]
fn shell_log_byte_budget_evicts_existing_logs_before_a_new_write() {
    let root = tempfile::tempdir().unwrap();
    let name = "shell-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.log";
    let file = std::fs::File::create(root.path().join(name)).unwrap();
    file.set_len(SHELL_OUTPUT_LOG_MAX_TOTAL_BYTES).unwrap();

    assert_eq!(
        cleanup_shell_output_logs_at(root.path(), std::time::SystemTime::now(), Some(1)).unwrap(),
        1
    );
    assert!(!root.path().join(name).exists());
}

#[test]
fn shell_output_logs_are_atomically_written_and_name_scoped() {
    let root = tempfile::tempdir().unwrap();
    let name = "shell-0123456789abcdef0123456789abcdef.log";
    write_shell_output_log_at(root.path(), name, "complete output").unwrap();
    assert_eq!(
        std::fs::read_to_string(root.path().join(name)).unwrap(),
        "complete output"
    );
    assert!(write_shell_output_log_at(root.path(), "../../outside.log", "bad").is_err());
}

#[test]
fn bounded_shell_output_keeps_a_utf8_safe_tail() {
    let text = "界".repeat(SHELL_OUTPUT_LOG_MAX_FILE_BYTES);
    let bounded = bounded_shell_output_log(&text);
    assert!(bounded.len() <= SHELL_OUTPUT_LOG_MAX_FILE_BYTES);
    assert!(bounded.starts_with(SHELL_OUTPUT_LOG_TRUNCATION_MARKER));
    assert!(bounded.is_char_boundary(bounded.len()));
}

#[test]
fn shell_child_receives_only_the_filtered_interactive_environment() {
    const PROBE: &str = "HAVEN_SHELL_ENVIRONMENT_PROBE";
    let mut command =
        std::process::Command::new(std::env::current_exe().expect("current test executable"));
    command.args([
        "--exact",
        "shell_runtime::tests::shell_environment_probe_child",
        "--nocapture",
    ]);
    configure_shell_environment(
        &mut command,
        [
            ("PATH".into(), "C:/runtime".into()),
            ("SYSTEMROOT".into(), "C:/Windows".into()),
            ("TEMP".into(), "C:/Temp".into()),
            ("TMP".into(), "C:/Temp".into()),
            ("USERPROFILE".into(), "C:/Users/Alice".into()),
            ("HTTPS_PROXY".into(), "http://proxy.invalid".into()),
            ("OPENAI_API_KEY".into(), "must-not-leak".into()),
            ("PYTHONPATH".into(), "untrusted-import-root".into()),
        ]
        .into_iter(),
        [],
    );
    command.env(PROBE, "1");

    let output = command_output(&mut command).expect("spawn shell environment probe");
    assert!(
        output.status.success(),
        "child environment probe failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn shell_environment_probe_child() {
    if std::env::var_os("HAVEN_SHELL_ENVIRONMENT_PROBE").is_none() {
        return;
    }
    assert_eq!(std::env::var("PATH").unwrap(), "C:/runtime");
    assert_eq!(std::env::var("USERPROFILE").unwrap(), "C:/Users/Alice");
    assert_eq!(
        std::env::var("HTTPS_PROXY").unwrap(),
        "http://proxy.invalid"
    );
    assert!(std::env::var_os("OPENAI_API_KEY").is_none());
    assert!(std::env::var_os("PYTHONPATH").is_none());
}

#[cfg(windows)]
#[test]
fn filtered_shell_environment_supports_common_windows_commands() {
    let work_dir = tempfile::tempdir().expect("create isolated shell work directory");

    let mut cmd = build_shell_command_silent("cmd", "echo haven-cmd-compat");
    cmd.current_dir(work_dir.path());
    let output = command_output(&mut cmd).expect("run cmd with filtered environment");
    assert!(
        output.status.success(),
        "cmd failed with filtered environment: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("haven-cmd-compat"));

    let script = r#"
$ErrorActionPreference = 'Stop'
if ([string]::IsNullOrWhiteSpace($env:SystemRoot)) { exit 31 }
if ([string]::IsNullOrWhiteSpace($env:TEMP) -or -not (Test-Path -LiteralPath $env:TEMP -PathType Container)) { exit 32 }
if ([string]::IsNullOrWhiteSpace($env:TMP) -or -not (Test-Path -LiteralPath $env:TMP -PathType Container)) { exit 33 }
if ([string]::IsNullOrWhiteSpace($env:USERPROFILE)) { exit 34 }
foreach ($name in @('cmd.exe', 'where.exe', 'whoami.exe')) {
    if (-not (Get-Command $name -ErrorAction SilentlyContinue)) { exit 35 }
}
& $env:ComSpec /d /c 'echo haven-cmd-child-compat' | Out-Null
if ($LASTEXITCODE -ne 0) { exit 36 }
$file = Join-Path (Get-Location) 'shell-compatibility.txt'
Set-Content -LiteralPath $file -Value 'haven-temp-workdir-compat'
if ((Get-Content -LiteralPath $file -Raw).Trim() -ne 'haven-temp-workdir-compat') { exit 37 }
Remove-Item -LiteralPath $file -Force
[Console]::Out.Write('haven-powershell-compat')
"#;
    let mut powershell = build_shell_command_silent("powershell", script);
    powershell.current_dir(work_dir.path());
    let output = command_output(&mut powershell).expect("run PowerShell with filtered environment");
    assert!(
        output.status.success(),
        "PowerShell failed with filtered environment (exit {:?}): {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("haven-powershell-compat"));
    assert!(
        !work_dir.path().join("shell-compatibility.txt").exists(),
        "PowerShell compatibility probe should clean its temporary file"
    );
}

fn command_output(command: &mut std::process::Command) -> std::io::Result<std::process::Output> {
    let child = {
        let _spawn_guard = haven_platform::process_containment::process_spawn_lock();
        command.spawn()?
    };
    child.wait_with_output()
}

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
        .as_chunks::<2>()
        .0
        .iter()
        .map(|chunk| u16::from_le_bytes(*chunk))
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
        .as_chunks::<2>()
        .0
        .iter()
        .map(|chunk| u16::from_le_bytes(*chunk))
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
