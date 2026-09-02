/// Windows `CREATE_NO_WINDOW` process creation flag: the child runs without a
/// console window. Single definition for every spawn site that must never pop
/// a console (background actions, silent shell commands, launched GUI-less
/// commands).
#[cfg(windows)]
pub const CREATE_NO_WINDOW: u32 = 0x08000000;

/// Build the platform command used to run `command` in the requested
/// interpreter (cmd or powershell), with stdout/stderr piped. Window
/// suppression (`CREATE_NO_WINDOW`) is applied here unconditionally because
/// background actions must never pop a console. The foreground `ShellTool`
/// uses `build_shell_command_silent` only when `silent` is requested, so
/// non-silent foreground commands can still show their window.
pub fn build_shell_command(shell: &str, command: &str) -> std::process::Command {
    let mut std_cmd = build_shell_command_silent(shell, command);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        std_cmd.creation_flags(CREATE_NO_WINDOW);
    }
    std_cmd
}

/// Interpreter selection + piped stdio WITHOUT the `CREATE_NO_WINDOW` flag.
/// Callers decide whether to suppress the console window.
pub fn build_shell_command_silent(shell: &str, command: &str) -> std::process::Command {
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
    // Default to the shared Temp working directory so the agent never executes
    // commands in the app's own working directory. Callers may override with
    // `.current_dir(...)` before spawning.
    std_cmd.current_dir(haven_common::default_work_dir());
    // Route git/npm/curl through a locally detected proxy so network-heavy
    // commands (clone/install) don't stall on ECONNRESET when the user runs a
    // local proxy (e.g. 127.0.0.1:10808). The probe is cached; env vars the
    // user already configured take precedence and are never overridden.
    for (key, val) in proxy_env_vars() {
        if std::env::var_os(&key).is_none() {
            std_cmd.env(key, val);
        }
    }
    std_cmd
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
pub fn proxy_env_vars() -> Vec<(String, String)> {
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
pub fn proxy_env_vars() -> Vec<(String, String)> {
    Vec::new()
}

/// Directory for per-command output logs (background actions and failed
/// foreground commands), under the shared Temp working directory.
pub fn output_log_dir(kind: &str) -> std::path::PathBuf {
    haven_common::default_work_dir().join(kind)
}

/// Write a command's full (sanitized) output to a log file so a condensed
/// failure summary never hides the root cause (e.g. an npm install failure
/// whose real error sits mid-log). Returns the log file path.
pub fn write_output_log(kind: &str, id: &str, text: &str) -> std::path::PathBuf {
    let dir = output_log_dir(kind);
    let path = dir.join(format!("{id}.log"));
    if let Err(e) = std::fs::create_dir_all(&dir) {
        tracing::warn!(action_id = %id, "failed to create output-log dir {}: {e}", dir.display());
        return path;
    }
    if let Err(e) = std::fs::write(&path, text) {
        tracing::warn!(action_id = %id, "failed to write output log {}: {e}", path.display());
    }
    path
}

/// Byte budget for collecting a command's combined stdout/stderr, derived
/// from the configured character cap (4 bytes/char worst case for UTF-8,
/// floored at 8 KiB). Shared by the foreground and background shell paths.
pub fn collect_byte_cap(max_chars: usize) -> usize {
    max_chars.saturating_mul(4).max(8192)
}

#[cfg(test)]
#[path = "shell_runtime_tests.rs"]
mod tests;
