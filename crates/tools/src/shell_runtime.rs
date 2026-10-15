use std::borrow::Cow;
use std::path::Path;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, SystemTime};

/// Windows `CREATE_NO_WINDOW` process creation flag. The contained-spawn owner
/// combines it with the suspended-process flag before spawning.
#[cfg(windows)]
pub(crate) const CREATE_NO_WINDOW: u32 = 0x08000000;

const SHELL_OUTPUT_LOG_RETENTION: Duration = Duration::from_secs(7 * 24 * 60 * 60);
const SHELL_OUTPUT_LOG_MAX_COUNT: usize = 128;
const SHELL_OUTPUT_LOG_MAX_TOTAL_BYTES: u64 = 32 * 1024 * 1024;
const SHELL_OUTPUT_LOG_MAX_FILE_BYTES: usize = 1024 * 1024;
const SHELL_OUTPUT_LOG_TRUNCATION_MARKER: &str = "[earlier shell output truncated]\n";

static SHELL_OUTPUT_LOG_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

/// Build the platform command used to run `command` in the requested
/// interpreter (cmd or powershell), with stdout/stderr piped. The process
/// owner supplies `CREATE_NO_WINDOW` when appropriate and combines it with
/// the suspended-process containment flag.
pub(crate) fn build_shell_command(shell: &str, command: &str) -> std::process::Command {
    build_shell_command_silent(shell, command)
}

/// Interpreter selection + piped stdio WITHOUT the `CREATE_NO_WINDOW` flag.
/// Callers decide whether to suppress the console window.
pub(crate) fn build_shell_command_silent(shell: &str, command: &str) -> std::process::Command {
    #[cfg(windows)]
    let mut std_cmd = match shell {
        // `powershell` (Windows built-in PS 5.1) and `pwsh` (PowerShell 7+)
        // share the same wrapper: -EncodedCommand plus forced UTF-8.
        "powershell" | "pwsh" => {
            // Force UTF-8 for both the native-command pipe ($OutputEncoding) and
            // PowerShell's own redirected output ([Console]::OutputEncoding) so
            // command output arrives as UTF-8 instead of the OEM/ANSI code page.
            // The Out-File default is also pinned to UTF-8: PS 5.1's `>`
            // redirection writes UTF-16LE otherwise, and files the agent later
            // reads (`cat`, Get-Content) would come back as mangled UTF-16.
            let mut c = std::process::Command::new(shell);
            // ProgressPreference: redirected stderr serializes ProgressRecords
            // as CLIXML ("正在准备首次使用模块" / Completed). Large commands
            // then surface that blob as the failure text after strip_xml_markup
            // concatenates type names. Silence progress so only real errors
            // reach the pipe.
            let ps = format!(
                "$ProgressPreference = 'SilentlyContinue'; \
                 $OutputEncoding = [Console]::OutputEncoding = [System.Text.Encoding]::UTF8; \
                 $PSDefaultParameterValues['Out-File:Encoding'] = 'utf8'; {command}"
            );
            // Pass the whole script via -EncodedCommand (UTF-16LE base64)
            // instead of -Command: the payload is a single opaque ASCII token,
            // so quotes, semicolons, `%`, backticks and `$` in the user command
            // can never be mangled by PowerShell's own command-line re-parsing,
            // and non-ASCII input survives the console code page unchanged.
            c.args([
                "-NoProfile",
                "-NonInteractive",
                "-EncodedCommand",
                encode_utf16le_base64(&ps).as_str(),
            ]);
            c
        }
        _ => {
            // chcp 65001 flips cmd's console code page to UTF-8. Byte output from
            // children passes through the pipe unaltered, so tools that ignore
            // the code page (still GBK) are decoded lossily by decode_lossy.
            let mut c = std::process::Command::new("cmd");
            let cmdline = format!("chcp 65001 >nul 2>nul & {command}");
            c.args(["/C", cmdline.as_str()]);
            c
        }
    };
    #[cfg(not(windows))]
    let mut std_cmd = match shell {
        "bash" => {
            let mut c = std::process::Command::new("bash");
            c.args(["-c", command]);
            c
        }
        // "sh" and unknown values fall back to POSIX sh; on Linux the shell
        // tool schema only advertises sh/bash anyway.
        _ => {
            let mut c = std::process::Command::new("sh");
            c.args(["-c", command]);
            c
        }
    };
    std_cmd.stdout(std::process::Stdio::piped());
    std_cmd.stderr(std::process::Stdio::piped());
    // Default to Haven's shared runtime Temp directory so the agent never executes
    // commands in the app's own working directory. Callers may override with
    // `.current_dir(...)` before spawning.
    std_cmd.current_dir(haven_common::config::default_runtime_temp_root());
    configure_shell_environment(&mut std_cmd, std::env::vars_os(), proxy_env_vars());
    std_cmd
}

fn configure_shell_environment(
    command: &mut std::process::Command,
    inherited_environment: impl Iterator<Item = (std::ffi::OsString, std::ffi::OsString)>,
    detected_proxy_variables: impl IntoIterator<Item = (String, String)>,
) {
    command
        .env_clear()
        .envs(haven_platform::process_environment::filter_shell_environment(inherited_environment));
    // Route git/npm/curl through a locally detected proxy so network-heavy
    // commands (clone/install) don't stall on ECONNRESET when the user runs a
    // local proxy (e.g. 127.0.0.1:10808). The probe is cached; explicitly
    // configured proxy variables remain in the filtered environment and take
    // precedence.
    for (key, val) in detected_proxy_variables {
        command.env(key, val);
    }
}

/// Base64-encode a string as UTF-16LE for PowerShell's `-EncodedCommand`.
///
/// PowerShell decodes the argument as UTF-16LE bytes, so this round-trips any
/// Unicode input exactly and stays pure ASCII on the process command line,
/// sidestepping both argument-escaping and console-code-page issues.
#[cfg(windows)]
fn encode_utf16le_base64(text: &str) -> String {
    use base64::Engine;
    let mut bytes = Vec::with_capacity(text.len() * 2);
    for unit in text.encode_utf16() {
        bytes.extend_from_slice(&unit.to_le_bytes());
    }
    base64::engine::general_purpose::STANDARD.encode(&bytes)
}

/// Detect a locally running proxy (common Windows proxy ports) and return
/// the env vars that route HTTP(S) traffic through it.
///
/// The probe (a short TCP connect per port) runs once and the result is
/// cached for 5 minutes, so the first shell command on a fresh process pays
/// a one-time ~100 ms cost at most. Env vars already present in the process
/// environment (user-configured proxy) short-circuit the probe entirely —
/// a detected local proxy must never override an explicit configuration.
#[cfg(windows)]
pub(crate) fn proxy_env_vars() -> Vec<(String, String)> {
    use std::net::{SocketAddr, TcpStream};
    use std::sync::OnceLock;
    use std::time::{Duration, Instant};

    struct Cache {
        probed_at: Instant,
        vars: Vec<(String, String)>,
    }
    static CACHE: OnceLock<std::sync::Mutex<Option<Cache>>> = OnceLock::new();

    if std::env::var_os("HTTP_PROXY").is_some()
        || std::env::var_os("HTTPS_PROXY").is_some()
        || std::env::var_os("http_proxy").is_some()
        || std::env::var_os("https_proxy").is_some()
    {
        return Vec::new();
    }

    let cache = CACHE.get_or_init(|| std::sync::Mutex::new(None));
    let mut guard = cache.lock().unwrap();
    if let Some(c) = guard.as_ref()
        && c.probed_at.elapsed() < Duration::from_secs(300)
    {
        return c.vars.clone();
    }

    let mut vars = Vec::new();
    for port in [10808, 10809, 7890, 7897, 1080] {
        let addr: SocketAddr = match format!("127.0.0.1:{port}").parse() {
            Ok(a) => a,
            Err(_) => continue,
        };
        if TcpStream::connect_timeout(&addr, Duration::from_millis(120)).is_ok() {
            let url = format!("http://127.0.0.1:{port}");
            for key in [
                "HTTP_PROXY",
                "HTTPS_PROXY",
                "ALL_PROXY",
                "http_proxy",
                "https_proxy",
                "all_proxy",
            ] {
                vars.push((key.to_string(), url.clone()));
            }
            break;
        }
    }
    guard.replace(Cache {
        probed_at: Instant::now(),
        vars: vars.clone(),
    });
    vars
}

#[cfg(not(windows))]
pub(crate) fn proxy_env_vars() -> Vec<(String, String)> {
    Vec::new()
}

/// Directory for per-command output logs (background ToolRuns and failed
/// foreground commands), under Haven's shared runtime Temp directory.
fn output_log_dir(kind: &str) -> std::path::PathBuf {
    haven_common::config::default_runtime_temp_root().join(kind)
}

/// Remove the full-output log owned by one deleted ToolRun. Never trust the
/// persisted `log_path`: the canonical ToolRun identity determines the only
/// leaf that this cleanup may remove.
pub(crate) fn remove_tool_run_output_log(tool_run_id: &str) {
    if !haven_common::types::is_canonical_id(tool_run_id, "toolrun") {
        tracing::warn!("skipping ToolRun log cleanup for a non-canonical id");
        return;
    }

    let directory = output_log_dir("tool-run-logs");
    match remove_tool_run_output_log_at(&directory, tool_run_id) {
        Ok(_) => {}
        Err(error) => tracing::warn!(
            tool_run_id,
            error = %haven_common::error::sanitize_error_text(&error.to_string()),
            "failed to remove deleted ToolRun output log"
        ),
    }
}

/// Remove orphaned generated ToolRun logs after durable history is loaded.
/// The open directory capability anchors deletion even if the path is later
/// replaced; unrecognized filenames are left untouched.
pub(crate) fn remove_orphaned_tool_run_output_logs(
    retained_tool_run_ids: &std::collections::HashSet<String>,
) -> std::io::Result<usize> {
    let directory = output_log_dir("tool-run-logs");
    remove_orphaned_tool_run_output_logs_at(&directory, retained_tool_run_ids)
}

/// Reconcile foreground Shell failure logs with their ephemeral seven-day
/// retention policy and hard directory budgets. These logs have no durable
/// ToolRun row that can own their lifetime.
#[cfg(not(test))]
pub(crate) fn cleanup_shell_output_logs_after_restart() {
    let lock = SHELL_OUTPUT_LOG_LOCK.get_or_init(|| Mutex::new(()));
    let _guard = lock
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let directory = output_log_dir("shell-logs");
    match cleanup_shell_output_logs_at(&directory, SystemTime::now(), None) {
        Ok(0) => {}
        Ok(removed) => tracing::debug!(
            removed,
            "removed expired or over-budget foreground Shell output logs"
        ),
        Err(error) => tracing::warn!(
            error = %haven_common::error::sanitize_error_text(&error.to_string()),
            "failed to clean foreground Shell output logs"
        ),
    }
}

#[cfg(test)]
pub(crate) fn write_shell_output_log_for_test(
    directory: &Path,
    file_stem: &str,
    text: &str,
) -> std::path::PathBuf {
    write_shell_output_log(directory, file_stem, text)
}

fn cleanup_shell_output_logs_at(
    directory: &Path,
    now: SystemTime,
    incoming_bytes: Option<usize>,
) -> std::io::Result<usize> {
    use haven_platform::filesystem::{DirectoryCapability, is_link_or_reparse_point};

    let capability = match DirectoryCapability::open(directory) {
        Ok(capability) => capability,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(error),
    };
    let entries = std::fs::read_dir(capability.root())?;
    let mut logs = Vec::new();
    let mut staging = Vec::new();
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name_text) = name.to_str() else {
            continue;
        };
        let is_log = is_managed_shell_output_log_name(name_text);
        let is_staging = is_atomic_staging_name(name_text);
        if !is_log && !is_staging {
            continue;
        }
        let metadata = match std::fs::symlink_metadata(entry.path()) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        if !metadata.is_file() || is_link_or_reparse_point(&metadata) {
            continue;
        }
        let modified = metadata.modified()?;
        let record = ShellOutputLogEntry {
            name,
            modified,
            bytes: metadata.len(),
        };
        if is_log {
            logs.push(record);
        } else {
            staging.push(record);
        }
    }

    let mut removed = 0;
    for entry in staging {
        remove_shell_log_entry(&capability, &entry.name)?;
        removed += 1;
    }

    let mut retained = Vec::with_capacity(logs.len());
    for entry in logs {
        if now.duration_since(entry.modified).unwrap_or_default() > SHELL_OUTPUT_LOG_RETENTION {
            remove_shell_log_entry(&capability, &entry.name)?;
            removed += 1;
        } else {
            retained.push(entry);
        }
    }
    retained.sort_by(|left, right| {
        left.modified
            .cmp(&right.modified)
            .then_with(|| left.name.cmp(&right.name))
    });

    let mut total_bytes = retained
        .iter()
        .fold(0u64, |total, entry| total.saturating_add(entry.bytes));
    let incoming_count = if incoming_bytes.is_some() { 1 } else { 0 };
    let incoming_bytes = incoming_bytes.map_or(0, |bytes| bytes as u64);
    while retained.len() + incoming_count > SHELL_OUTPUT_LOG_MAX_COUNT
        || total_bytes.saturating_add(incoming_bytes) > SHELL_OUTPUT_LOG_MAX_TOTAL_BYTES
    {
        if retained.is_empty() {
            break;
        }
        let oldest = retained.remove(0);
        total_bytes = total_bytes.saturating_sub(oldest.bytes);
        remove_shell_log_entry(&capability, &oldest.name)?;
        removed += 1;
    }
    Ok(removed)
}

struct ShellOutputLogEntry {
    name: std::ffi::OsString,
    modified: SystemTime,
    bytes: u64,
}

fn remove_shell_log_entry(
    capability: &haven_platform::filesystem::DirectoryCapability,
    name: &std::ffi::OsStr,
) -> std::io::Result<()> {
    match capability.remove_file(Path::new(name)) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn is_shell_output_log_name(name: &str) -> bool {
    shell_output_log_nonce(name).is_some_and(|nonce| {
        nonce.len() == 32
            && nonce
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}

fn is_managed_shell_output_log_name(name: &str) -> bool {
    is_shell_output_log_name(name)
        || shell_output_log_nonce(name).is_some_and(|nonce| {
            !nonce.is_empty() && nonce.bytes().all(|byte| byte.is_ascii_digit())
        })
}

fn shell_output_log_nonce(name: &str) -> Option<&str> {
    name.strip_suffix(".log")?.strip_prefix("shell-")
}

fn is_atomic_staging_name(name: &str) -> bool {
    let Some(nonce) = name
        .strip_prefix(".file-")
        .and_then(|name| name.strip_suffix(".tmp"))
    else {
        return false;
    };
    nonce.len() == 32
        && nonce
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn bounded_shell_output_log(text: &str) -> Cow<'_, str> {
    if text.len() <= SHELL_OUTPUT_LOG_MAX_FILE_BYTES {
        return Cow::Borrowed(text);
    }
    let tail_budget = SHELL_OUTPUT_LOG_MAX_FILE_BYTES - SHELL_OUTPUT_LOG_TRUNCATION_MARKER.len();
    let mut tail_start = text.len() - tail_budget;
    while !text.is_char_boundary(tail_start) {
        tail_start += 1;
    }
    let mut bounded = String::with_capacity(SHELL_OUTPUT_LOG_MAX_FILE_BYTES);
    bounded.push_str(SHELL_OUTPUT_LOG_TRUNCATION_MARKER);
    bounded.push_str(&text[tail_start..]);
    Cow::Owned(bounded)
}

fn write_shell_output_log_at(directory: &Path, file_name: &str, text: &str) -> std::io::Result<()> {
    if !is_shell_output_log_name(file_name) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "foreground Shell output log name is not canonical",
        ));
    }
    let capability = haven_platform::filesystem::DirectoryCapability::open(directory)?;
    capability.atomic_replace(Path::new(file_name), text.as_bytes())
}

fn remove_tool_run_output_log_at(
    directory: &std::path::Path,
    tool_run_id: &str,
) -> std::io::Result<bool> {
    if !haven_common::types::is_canonical_id(tool_run_id, "toolrun") {
        return Ok(false);
    }

    let capability = match haven_platform::filesystem::DirectoryCapability::open(directory) {
        Ok(capability) => capability,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error),
    };
    match capability.remove_file(std::path::Path::new(&format!("{tool_run_id}.log"))) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

fn remove_orphaned_tool_run_output_logs_at(
    directory: &std::path::Path,
    retained_tool_run_ids: &std::collections::HashSet<String>,
) -> std::io::Result<usize> {
    let capability = match haven_platform::filesystem::DirectoryCapability::open(directory) {
        Ok(capability) => capability,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(error),
    };
    let root = capability.root().to_path_buf();
    let entries = match std::fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(error),
    };

    let mut removed = 0;
    for entry in entries {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            continue;
        }
        let path = entry.path();
        if path.extension().and_then(std::ffi::OsStr::to_str) != Some("log") {
            continue;
        }
        let Some(tool_run_id) = path.file_stem().and_then(std::ffi::OsStr::to_str) else {
            continue;
        };
        if !haven_common::types::is_canonical_id(tool_run_id, "toolrun")
            || retained_tool_run_ids.contains(tool_run_id)
        {
            continue;
        }

        match capability.remove_file(std::path::Path::new(&format!("{tool_run_id}.log"))) {
            Ok(()) => removed += 1,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    Ok(removed)
}

/// Write a command's captured output to a log file so a condensed
/// failure summary never hides the root cause (e.g. an npm install failure
/// whose real error sits mid-log). Returns the log file path.
pub(crate) fn write_output_log(kind: &str, file_stem: &str, text: &str) -> std::path::PathBuf {
    let dir = output_log_dir(kind);
    let path = dir.join(format!("{file_stem}.log"));
    if kind == "shell-logs" {
        return write_shell_output_log(&dir, file_stem, text);
    }
    if let Err(e) = std::fs::create_dir_all(&dir) {
        tracing::warn!(
            output_log_stem = %file_stem,
            error = %haven_common::error::sanitize_error_text(&e.to_string()),
            "failed to create output-log dir"
        );
        return path;
    }
    if let Err(e) = std::fs::write(&path, text) {
        tracing::warn!(
            output_log_stem = %file_stem,
            error = %haven_common::error::sanitize_error_text(&e.to_string()),
            "failed to write output log"
        );
    }
    path
}

fn write_shell_output_log(directory: &Path, file_stem: &str, text: &str) -> std::path::PathBuf {
    let file_name = format!("{file_stem}.log");
    let path = directory.join(&file_name);
    if !is_shell_output_log_name(&file_name) {
        tracing::warn!(output_log_stem = %file_stem, "rejected non-canonical foreground Shell output-log name");
        return path;
    }
    if let Err(error) = std::fs::create_dir_all(directory) {
        tracing::warn!(
            output_log_stem = %file_stem,
            error = %haven_common::error::sanitize_error_text(&error.to_string()),
            "failed to create foreground Shell output-log dir"
        );
        return path;
    }
    let lock = SHELL_OUTPUT_LOG_LOCK.get_or_init(|| Mutex::new(()));
    let _guard = lock
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let bounded = bounded_shell_output_log(text);
    if let Err(error) =
        cleanup_shell_output_logs_at(directory, SystemTime::now(), Some(bounded.len()))
    {
        tracing::warn!(
            output_log_stem = %file_stem,
            error = %haven_common::error::sanitize_error_text(&error.to_string()),
            "failed to enforce foreground Shell output-log budget"
        );
        return path;
    }
    if let Err(error) = write_shell_output_log_at(directory, &file_name, &bounded) {
        tracing::warn!(
            output_log_stem = %file_stem,
            error = %haven_common::error::sanitize_error_text(&error.to_string()),
            "failed to write foreground Shell output log"
        );
    }
    path
}

/// Byte budget for collecting a command's combined stdout/stderr, derived
/// from the configured character cap (4 bytes/char worst case for UTF-8,
/// floored at 8 KiB). Shared by the foreground and background shell paths.
pub(crate) fn collect_byte_cap(max_chars: usize) -> usize {
    max_chars.saturating_mul(4).max(8192)
}

#[cfg(test)]
#[path = "shell_runtime_tests.rs"]
mod tests;
