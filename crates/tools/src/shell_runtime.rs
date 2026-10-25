use haven_platform::filesystem::{DirectoryCapability, DirectoryEntryKind};
use std::borrow::Cow;
use std::path::{Path, PathBuf};
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
const SHELL_SCRATCH_DIRECTORY: &str = "shell-scratch";
const SHELL_CACHE_DIRECTORY: &str = "shell-cache";
const SHELL_SCRATCH_MAX_BYTES: u64 = 256 * 1024 * 1024;
const SHELL_NPM_CACHE_MAX_BYTES: u64 = 512 * 1024 * 1024;
const SHELL_CARGO_HOME_MAX_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const SHELL_SCRATCH_MAX_CONCURRENT: usize = 8;
const SHELL_SCRATCH_POOL_MAX_BYTES: u64 =
    SHELL_SCRATCH_MAX_BYTES * SHELL_SCRATCH_MAX_CONCURRENT as u64;
const SHELL_CAPACITY_CHECK_INTERVAL: Duration = Duration::from_secs(1);
const SHELL_SHARED_CAPACITY_CHECK_INTERVAL: Duration = Duration::from_secs(15);
const SHELL_CAPACITY_MAX_ENTRIES: usize = 100_000;
const SHELL_CACHE_CAPACITY_MAX_ENTRIES: usize = 1_000_000;
const LSP_SCRATCH_DIRECTORY: &str = "lsp-scratch";
pub(crate) const LSP_SCRATCH_MAX_BYTES: u64 = 256 * 1024 * 1024;
pub(crate) const LSP_SCRATCH_MAX_ENTRIES: usize = 100_000;

static SHELL_OUTPUT_LOG_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
static SHELL_CAPACITY_SCAN_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
static SHELL_SCRATCH_SLOTS: tokio::sync::Semaphore =
    tokio::sync::Semaphore::const_new(SHELL_SCRATCH_MAX_CONCURRENT);

pub(crate) struct ProcessScratchDirectory {
    scratch: tempfile::TempDir,
}

impl ProcessScratchDirectory {
    pub(crate) fn create_lsp() -> anyhow::Result<Self> {
        let root = haven_common::config::default_runtime_temp_root().join(LSP_SCRATCH_DIRECTORY);
        std::fs::create_dir_all(&root)?;
        DirectoryCapability::open(&root)?;
        let scratch = tempfile::Builder::new().prefix("lsp-").tempdir_in(&root)?;
        Ok(Self { scratch })
    }

    pub(crate) fn path(&self) -> &Path {
        self.scratch.path()
    }

    pub(crate) async fn ensure_within_capacity(&self) -> anyhow::Result<()> {
        let path = self.scratch.path().to_path_buf();
        let over_budget = tokio::task::spawn_blocking(move || {
            directory_size_exceeds(&path, LSP_SCRATCH_MAX_BYTES, LSP_SCRATCH_MAX_ENTRIES)
        })
        .await
        .map_err(|_| anyhow::anyhow!("LSP scratch capacity worker failed"))?
        .map_err(anyhow::Error::msg)?;
        anyhow::ensure!(!over_budget, "LSP scratch exceeded its capacity limit");
        Ok(())
    }
}

pub(crate) struct ShellRuntimeDirectories {
    scratch: tempfile::TempDir,
    scratch_root: PathBuf,
    npm_cache: PathBuf,
    cargo_home: PathBuf,
}

impl ShellRuntimeDirectories {
    pub(crate) fn create() -> anyhow::Result<Self> {
        let runtime_root = haven_common::config::default_runtime_temp_root();
        let scratch_root = runtime_root.join(SHELL_SCRATCH_DIRECTORY);
        let cache_root = runtime_root.join(SHELL_CACHE_DIRECTORY);
        let npm_cache = cache_root.join("npm");
        let cargo_home = cache_root.join("cargo");

        std::fs::create_dir_all(&scratch_root)?;
        std::fs::create_dir_all(&npm_cache)?;
        std::fs::create_dir_all(&cargo_home)?;
        DirectoryCapability::open(&scratch_root)?;
        DirectoryCapability::open(&npm_cache)?;
        DirectoryCapability::open(&cargo_home)?;

        let scratch = tempfile::Builder::new()
            .prefix("scratch-")
            .tempdir_in(&scratch_root)?;
        Ok(Self {
            scratch,
            scratch_root,
            npm_cache,
            cargo_home,
        })
    }

    pub(crate) fn scratch_path(&self) -> &Path {
        self.scratch.path()
    }

    pub(crate) fn cache_paths(&self) -> [&Path; 2] {
        [&self.npm_cache, &self.cargo_home]
    }

    pub(crate) fn apply_to_command(&self, command: &mut std::process::Command) {
        for key in ["TEMP", "TMP", "TMPDIR", "npm_config_cache", "CARGO_HOME"] {
            let overridden_keys = command
                .get_envs()
                .filter(|(candidate, _)| {
                    candidate
                        .to_str()
                        .is_some_and(|candidate| candidate.eq_ignore_ascii_case(key))
                })
                .map(|(candidate, _)| candidate.to_os_string())
                .collect::<Vec<_>>();
            for overridden_key in overridden_keys {
                command.env_remove(overridden_key);
            }
        }
        command
            .env("TEMP", self.scratch.path())
            .env("TMP", self.scratch.path())
            .env("TMPDIR", self.scratch.path())
            .env("npm_config_cache", &self.npm_cache)
            .env("CARGO_HOME", &self.cargo_home);
    }

    pub(crate) async fn ensure_within_capacity(&self) -> anyhow::Result<()> {
        if let Some(reason) = self.capacity_violation(true).await? {
            anyhow::bail!(reason);
        }
        Ok(())
    }

    pub(crate) async fn wait_for_capacity_violation(&self) -> String {
        let mut last_shared_check = tokio::time::Instant::now();
        loop {
            tokio::time::sleep(SHELL_CAPACITY_CHECK_INTERVAL).await;
            let check_shared = last_shared_check.elapsed() >= SHELL_SHARED_CAPACITY_CHECK_INTERVAL;
            if check_shared {
                last_shared_check = tokio::time::Instant::now();
            }
            match self.capacity_violation(check_shared).await {
                Ok(Some(reason)) => return reason,
                Ok(None) => {}
                Err(error) => return error.to_string(),
            }
        }
    }

    /// Reset an over-budget shared package cache only when no other Shell
    /// invocation can be using it. If another call is active, restart cleanup
    /// handles the cache after all child processes have stopped.
    pub(crate) async fn reset_over_budget_caches_if_idle(&self) {
        let Ok(_all_shell_slots) =
            SHELL_SCRATCH_SLOTS.try_acquire_many(SHELL_SCRATCH_MAX_CONCURRENT as u32)
        else {
            return;
        };
        let npm_cache = self.npm_cache.clone();
        let cargo_home = self.cargo_home.clone();
        let reset_result = tokio::task::spawn_blocking(move || {
            let _scan_lock = SHELL_CAPACITY_SCAN_LOCK
                .get_or_init(|| Mutex::new(()))
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            for (name, path, limit) in [
                ("npm", npm_cache, SHELL_NPM_CACHE_MAX_BYTES),
                ("Cargo", cargo_home, SHELL_CARGO_HOME_MAX_BYTES),
            ] {
                let over_budget =
                    directory_size_exceeds(&path, limit, SHELL_CACHE_CAPACITY_MAX_ENTRIES)
                        .unwrap_or(true);
                if over_budget {
                    reset_owned_directory(&path).map_err(|error| {
                        format!("failed to reset over-budget {name} cache: {error}")
                    })?;
                }
            }
            Ok::<_, String>(())
        })
        .await;
        match reset_result {
            Ok(Ok(())) => {}
            Ok(Err(error)) => tracing::warn!(
                error = %haven_common::error::sanitize_error_text(&error),
                "failed to reset an over-budget Shell package cache"
            ),
            Err(error) => tracing::warn!(
                error = %haven_common::error::sanitize_error_text(&error.to_string()),
                "failed to inspect Shell package cache after capacity stop"
            ),
        }
    }

    async fn capacity_violation(&self, check_shared: bool) -> anyhow::Result<Option<String>> {
        let scratch = self.scratch.path().to_path_buf();
        let scratch_root = self.scratch_root.clone();
        let npm_cache = self.npm_cache.clone();
        let cargo_home = self.cargo_home.clone();
        tokio::task::spawn_blocking(move || {
            let _shared_scan = check_shared.then(|| {
                SHELL_CAPACITY_SCAN_LOCK
                    .get_or_init(|| Mutex::new(()))
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
            });
            let mut roots = vec![(
                scratch,
                SHELL_SCRATCH_MAX_BYTES,
                SHELL_CAPACITY_MAX_ENTRIES,
                "Shell scratch",
            )];
            if check_shared {
                roots.extend([
                    (
                        scratch_root,
                        SHELL_SCRATCH_POOL_MAX_BYTES,
                        SHELL_CAPACITY_MAX_ENTRIES,
                        "Shell scratch pool",
                    ),
                    (
                        npm_cache,
                        SHELL_NPM_CACHE_MAX_BYTES,
                        SHELL_CACHE_CAPACITY_MAX_ENTRIES,
                        "npm cache",
                    ),
                    (
                        cargo_home,
                        SHELL_CARGO_HOME_MAX_BYTES,
                        SHELL_CACHE_CAPACITY_MAX_ENTRIES,
                        "Cargo cache",
                    ),
                ]);
            }
            for (path, limit, max_entries, name) in roots {
                if directory_size_exceeds(&path, limit, max_entries).map_err(anyhow::Error::msg)? {
                    return Ok(Some(format!(
                        "{name} exceeded its {} MiB capacity limit; the Shell process was stopped",
                        limit / (1024 * 1024)
                    )));
                }
            }
            Ok::<_, anyhow::Error>(None)
        })
        .await
        .map_err(|error| anyhow::anyhow!("failed to check Shell runtime capacity: {error}"))?
    }
}

pub(crate) fn shell_scratch_slots() -> &'static tokio::sync::Semaphore {
    &SHELL_SCRATCH_SLOTS
}

/// Build the platform command used to run `command` in the requested
/// interpreter (cmd or powershell), with stdout/stderr piped. The process
/// owner supplies `CREATE_NO_WINDOW` when appropriate and combines it with
/// the suspended-process containment flag.
#[cfg(test)]
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

/// Remove scratch directories left behind by a process crash and reset a
/// package cache if it exceeds its bounded runtime budget. This runs during
/// ToolRun restoration, before new shell calls are admitted.
#[cfg(not(test))]
pub(crate) fn cleanup_shell_runtime_directories_after_restart() {
    let runtime_root = haven_common::config::default_runtime_temp_root();
    let scratch_root = runtime_root.join(SHELL_SCRATCH_DIRECTORY);
    match cleanup_owned_scratch_directories(&scratch_root, "scratch-") {
        Ok(0) => {}
        Ok(removed) => tracing::debug!(removed, "removed stale Shell scratch directories"),
        Err(error) => tracing::warn!(
            error = %haven_common::error::sanitize_error_text(&error.to_string()),
            "failed to clean stale Shell scratch directories"
        ),
    }

    let lsp_scratch_root = runtime_root.join(LSP_SCRATCH_DIRECTORY);
    match cleanup_owned_scratch_directories(&lsp_scratch_root, "lsp-") {
        Ok(0) => {}
        Ok(removed) => tracing::debug!(removed, "removed stale LSP scratch directories"),
        Err(error) => tracing::warn!(
            error = %haven_common::error::sanitize_error_text(&error.to_string()),
            "failed to clean stale LSP scratch directories"
        ),
    }

    let cache_root = runtime_root.join(SHELL_CACHE_DIRECTORY);
    for (name, path, limit) in [
        ("npm", cache_root.join("npm"), SHELL_NPM_CACHE_MAX_BYTES),
        (
            "Cargo",
            cache_root.join("cargo"),
            SHELL_CARGO_HOME_MAX_BYTES,
        ),
    ] {
        let over_budget =
            match directory_size_exceeds(&path, limit, SHELL_CACHE_CAPACITY_MAX_ENTRIES) {
                Ok(over_budget) => over_budget,
                Err(error) => {
                    tracing::warn!(
                        cache = name,
                        error = %haven_common::error::sanitize_error_text(&error),
                        "failed to inspect Shell package cache after restart"
                    );
                    true
                }
            };
        if over_budget {
            match reset_owned_directory(&path) {
                Ok(()) => tracing::warn!(
                    cache = name,
                    limit_bytes = limit,
                    "reset over-budget Shell package cache after restart"
                ),
                Err(error) => tracing::warn!(
                    cache = name,
                    error = %haven_common::error::sanitize_error_text(&error.to_string()),
                    "failed to reset over-budget Shell package cache"
                ),
            }
        }
    }
}

pub(crate) fn directory_size_exceeds(
    root: &Path,
    limit: u64,
    max_entries: usize,
) -> Result<bool, String> {
    let root_metadata = match std::fs::symlink_metadata(root) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.to_string()),
    };
    if haven_platform::filesystem::is_link_or_reparse_point(&root_metadata)
        || !root_metadata.is_dir()
    {
        return Err("runtime directory is not a safe local directory".into());
    }
    DirectoryCapability::open(root)
        .map_err(|_| "runtime directory could not be safely opened".to_owned())?;

    let mut pending = vec![root.to_path_buf()];
    let mut total_bytes = 0u64;
    let mut entry_count = 0usize;
    while let Some(directory) = pending.pop() {
        let entries = match std::fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => {
                return Err("runtime directory could not be inspected safely".into());
            }
        };
        for entry in entries {
            let entry = entry.map_err(|_| "runtime directory could not be inspected safely")?;
            entry_count = entry_count.saturating_add(1);
            if entry_count > max_entries {
                return Ok(true);
            }
            let metadata = match std::fs::symlink_metadata(entry.path()) {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(_) => {
                    return Err("runtime directory could not be inspected safely".into());
                }
            };
            let file_type = metadata.file_type();
            if file_type.is_symlink()
                || haven_platform::filesystem::is_link_or_reparse_point(&metadata)
            {
                continue;
            }
            if metadata.is_dir() {
                pending.push(entry.path());
            } else if metadata.is_file() {
                total_bytes = total_bytes.saturating_add(metadata.len());
                if total_bytes > limit {
                    return Ok(true);
                }
            }
        }
    }
    Ok(false)
}

#[cfg(not(test))]
fn cleanup_owned_scratch_directories(root: &Path, prefix: &str) -> std::io::Result<usize> {
    let root = match DirectoryCapability::open(root) {
        Ok(root) => root,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(error),
    };
    let mut removed = 0;
    for entry in root.entries(SHELL_CAPACITY_MAX_ENTRIES)? {
        let name = entry.name;
        let Some(name) = name.to_str() else {
            continue;
        };
        if entry.kind != DirectoryEntryKind::Directory || !name.starts_with(prefix) {
            continue;
        }
        root.remove_dir_all(Path::new(name))?;
        removed += 1;
    }
    Ok(removed)
}

fn reset_owned_directory(path: &Path) -> std::io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| std::io::Error::other("Shell cache directory has no parent"))?;
    let name = path
        .file_name()
        .ok_or_else(|| std::io::Error::other("Shell cache directory has no name"))?;
    let relative = Path::new(name);
    let parent = DirectoryCapability::open(parent)?;
    if let Some(entry) = parent
        .entries(SHELL_CAPACITY_MAX_ENTRIES)?
        .into_iter()
        .find(|entry| entry.name.as_os_str() == name)
    {
        match entry.kind {
            DirectoryEntryKind::Directory => parent.remove_dir_all(relative)?,
            DirectoryEntryKind::File => parent.remove_file(relative)?,
            DirectoryEntryKind::ReparsePoint | DirectoryEntryKind::Other => {
                return Err(std::io::Error::other(
                    "refusing to reset a Shell cache reparse point or special file",
                ));
            }
        }
    }
    parent.create_dir_all(relative)
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
