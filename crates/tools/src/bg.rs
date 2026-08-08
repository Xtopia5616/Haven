use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::sync::{RwLock, mpsc, oneshot};

/// Maximum concurrent *running* background jobs per process. Prevents an
/// agent from leaking unbounded child processes. Finished jobs are reaped
/// on the next spawn, so this is a concurrency cap, not a lifetime cap.
/// Build the platform command used to run `command` in the requested
/// interpreter (cmd or powershell), with stdout/stderr piped. Window
/// suppression (`CREATE_NO_WINDOW`) is applied here unconditionally because
/// background jobs must never pop a console. The foreground `ShellTool`
/// uses `build_shell_command_silent` only when `silent` is requested, so
/// non-silent foreground commands can still show their window.
pub fn build_shell_command(shell: &str, command: &str) -> std::process::Command {
    let mut std_cmd = build_shell_command_silent(shell, command);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x08000000;
        std_cmd.creation_flags(CREATE_NO_WINDOW);
    }
    std_cmd
}

/// Interpreter selection + piped stdio WITHOUT the `CREATE_NO_WINDOW` flag.
/// Callers decide whether to suppress the console window.
pub fn build_shell_command_silent(shell: &str, command: &str) -> std::process::Command {
    #[cfg(windows)]
    let mut std_cmd = match shell {
        "powershell" => {
            let mut c = std::process::Command::new("powershell");
            c.args(["-NoProfile", "-Command", command]);
            c
        }
        _ => {
            let mut c = std::process::Command::new("cmd");
            c.args(["/C", command]);
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

/// Directory for per-command output logs (background jobs and failed
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
        tracing::warn!("failed to create output-log dir {}: {e}", dir.display());
        return path;
    }
    if let Err(e) = std::fs::write(&path, text) {
        tracing::warn!("failed to write output log {}: {e}", path.display());
    }
    path
}

/// Byte budget for collecting a command's combined stdout/stderr, derived
/// from the configured character cap (4 bytes/char worst case for UTF-8,
/// floored at 8 KiB). Shared by the foreground and background shell paths.
pub fn collect_byte_cap(max_chars: usize) -> usize {
    max_chars.saturating_mul(4).max(8192)
}

/// A background job that has reached a terminal state, surfaced to a consumer
/// (the agent layer) so the owning task can be auto-notified of the result
/// instead of the model having to poll `status`.
#[derive(Clone, Debug)]
pub struct JobCompletion {
    pub job_id: String,
    pub task_id: Option<String>,
    /// Terminal status string: "completed", "failed", or "cancelled".
    pub status: String,
    /// The job's status JSON (same shape `status()` returns for terminal
    /// states), carrying the output/error payload.
    pub status_json: Value,
}

#[derive(Clone, Debug)]
enum JobState {
    Running {
        started_at: String,
    },
    Completed {
        output: String,
        exit_code: Option<i32>,
        truncated: bool,
        /// Path to the full-output log file (written when output was capped).
        log_path: Option<String>,
        started_at: String,
        finished_at: String,
    },
    Failed {
        error: String,
        error_reason: String,
        /// Path to the full-output log file (always written for failures so
        /// the root cause survives the condensed `error_reason`).
        log_path: Option<String>,
        exit_code: Option<i32>,
        started_at: String,
        finished_at: String,
    },
    Cancelled {
        started_at: String,
        finished_at: String,
    },
}

impl JobState {
    fn is_terminal(&self) -> bool {
        !matches!(self, JobState::Running { .. })
    }
}

struct JobEntry {
    task_id: Option<String>,
    state: JobState,
    /// Kill signal for the running child process.
    kill: Option<oneshot::Sender<()>>,
}

/// Registry of background tool jobs (refine: long-running commands).
///
/// A job is spawned with `spawn_shell`, runs detached from the ReAct loop,
/// and is polled with `status`. Jobs are tied to a task via `attach_task`;
/// `cancel_for_task` kills and drops them when the task ends.
///
/// When a job finishes, a `JobCompletion` is sent on the completion channel
/// (see `take_completion_receiver`) so the agent layer can auto-inject the
/// result into the owning task's context without the model polling.
pub struct BackgroundJobs {
    jobs: RwLock<HashMap<String, JobEntry>>,
    completion_tx: mpsc::UnboundedSender<JobCompletion>,
    /// Receiver handed out exactly once to the consumer (the agent layer).
    completion_rx: Mutex<Option<mpsc::UnboundedReceiver<JobCompletion>>>,
    /// Max concurrent *running* jobs (from `context_limits.background_max_jobs`).
    max_jobs: RwLock<usize>,
}

impl Default for BackgroundJobs {
    fn default() -> Self {
        Self::new()
    }
}

impl BackgroundJobs {
    pub fn new() -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        Self {
            jobs: RwLock::new(HashMap::new()),
            completion_tx: tx,
            completion_rx: Mutex::new(Some(rx)),
            max_jobs: RwLock::new(64),
        }
    }

    /// Replace the unified context limits (background job concurrency cap).
    pub async fn set_limits(&self, limits: &haven_common::config::ContextLimitsConfig) {
        *self.max_jobs.write().await = limits.background_max_jobs;
    }

    /// Take the completion receiver exactly once. The caller spawns a consumer
    /// loop that receives `JobCompletion`s and notifies the owning tasks.
    /// Returns `None` if already taken.
    pub fn take_completion_receiver(&self) -> Option<mpsc::UnboundedReceiver<JobCompletion>> {
        self.completion_rx.lock().unwrap().take()
    }

    /// Emit a completion notification for a job (if it has a terminal state),
    /// reading the owning task_id from the entry. Called from `mark_finished`,
    /// `mark_cancelled`, and `attach_task` (the latter to close the race where
    /// a job finishes before its task binding is recorded).
    fn notify_completion(&self, job_id: &str, entry: &JobEntry) {
        if !entry.state.is_terminal() {
            return;
        }
        let status = match &entry.state {
            JobState::Completed { .. } => "completed",
            JobState::Failed { .. } => "failed",
            JobState::Cancelled { .. } => "cancelled",
            JobState::Running { .. } => return,
        };
        let status_json = render_status_json(job_id, &entry.state);
        let _ = self.completion_tx.send(JobCompletion {
            job_id: job_id.to_string(),
            task_id: entry.task_id.clone(),
            status: status.to_string(),
            status_json,
        });
    }

    /// Board view of every job owned by `task_id`: one entry per job with
    /// status, timestamps, and a bounded output/error preview. Lets the model
    /// see all background work of a task in a single call instead of polling
    /// `status` job by job. Order: oldest first.
    pub async fn list_for_task(&self, task_id: &str) -> Vec<Value> {
        let jobs = self.jobs.read().await;
        let mut rows = Vec::new();
        for (id, entry) in jobs.iter() {
            if entry.task_id.as_deref() != Some(task_id) {
                continue;
            }
            let mut row = match &entry.state {
                JobState::Running { started_at } => json!({
                    "job_id": id,
                    "status": "running",
                    "started_at": started_at,
                }),
                _ => render_status_json(id, &entry.state),
            };
            let preview = row
                .get("output")
                .and_then(|v| v.as_str())
                .or_else(|| row.get("error").and_then(|v| v.as_str()))
                .unwrap_or("");
            row["preview"] = json!(preview.chars().take(200).collect::<String>());
            rows.push(row);
        }
        rows.sort_by(|a, b| a["started_at"].as_str().cmp(&b["started_at"].as_str()));
        rows
    }

    /// Spawn a shell command as a background job. Returns the job id; the
    /// command keeps running after this function returns. `cwd` overrides the
    /// default Temp working directory when provided.
    pub async fn spawn_shell(
        self: &Arc<Self>,
        command: &str,
        shell: &str,
        max_chars: usize,
        cwd: Option<std::path::PathBuf>,
    ) -> anyhow::Result<String> {
        if command.trim().is_empty() {
            anyhow::bail!("command is required");
        }
        // Unpredictable job id: a sequential counter would let any task's
        // agent enumerate and read other tasks' background outputs through
        // status (which is RiskLevel::Safe).
        let id = format!("job-{}", uuid::Uuid::new_v4().simple());
        let started_at = chrono::Utc::now().to_rfc3339();
        let (kill_tx, kill_rx) = oneshot::channel();
        {
            let mut jobs = self.jobs.write().await;
            // Reap terminal entries first: their results were already
            // delivered via the completion channel, so they must not occupy
            // the cap forever (64 lifetime jobs would otherwise brick the
            // feature for long-lived tasks).
            jobs.retain(|_, e| !e.state.is_terminal());
            let running = jobs
                .values()
                .filter(|e| matches!(e.state, JobState::Running { .. }))
                .count();
            if running >= *self.max_jobs.read().await {
                anyhow::bail!(
                    "too many running background jobs (limit {})",
                    *self.max_jobs.read().await
                );
            }
            jobs.insert(
                id.clone(),
                JobEntry {
                    task_id: None,
                    state: JobState::Running {
                        started_at: started_at.clone(),
                    },
                    kill: Some(kill_tx),
                },
            );
        }

        let mut std_cmd = build_shell_command(shell, command);
        if let Some(cwd) = cwd {
            std_cmd.current_dir(cwd);
        }

        let mut child = match tokio::process::Command::from(std_cmd)
            .kill_on_drop(true)
            .spawn()
        {
            Ok(c) => c,
            Err(e) => {
                // Spawn failed: remove the entry so the job is not left
                // dangling as "running".
                self.jobs.write().await.remove(&id);
                return Err(e.into());
            }
        };

        let me = self.clone();
        let job_id = id.clone();
        let shell_owned = shell.to_string();
        let command_owned = command.to_string();
        // The direct child pid is captured before `run` moves `child`; on
        // Windows, cancelling must kill the whole process tree, not just the
        // cmd.exe/powershell.exe wrapper.
        let child_pid = child.id();
        tokio::spawn(async move {
            // The job outlives this task: when `run` is dropped (kill signal
            // received), kill_on_drop terminates the child.
            let max_collect = collect_byte_cap(max_chars);
            let stdout_fut = read_stream_capped(child.stdout.take(), max_collect);
            let stderr_fut = read_stream_capped(child.stderr.take(), max_collect);
            let run = async {
                let ((stdout, stdout_overflow), (stderr, stderr_overflow)) =
                    tokio::join!(stdout_fut, stderr_fut);
                let status = child.wait().await;
                let mut combined = stdout;
                if !stderr.is_empty() {
                    if !combined.is_empty() {
                        combined.push('\n');
                    }
                    combined.push_str(&stderr);
                }
                // Strip PowerShell's NativeCommandError/CLIXML formatting so
                // the payload carries the real message, not the noise.
                combined = sanitize_shell_output(&combined, &shell_owned);
                let exit_code = status.as_ref().ok().and_then(|s| s.code());
                let success = matches!(status, Ok(s) if s.success());
                let truncated = stdout_overflow || stderr_overflow;
                (combined, success, exit_code, truncated)
            };
            tokio::pin!(run);
            tokio::select! {
                _ = kill_rx => {
                    // Dropping `run` drops the pipes and the child
                    // (kill_on_drop), terminating the command.
                    if let Some(pid) = child_pid {
                        kill_process_tree(pid).await;
                    }
                    me.mark_cancelled(&job_id, &started_at).await;
                }
                (combined, success, exit_code, truncated) = &mut run => {
                    me.mark_finished(&job_id, &started_at, &shell_owned, &command_owned, combined, success, exit_code, truncated).await;
                }
            }
        });

        Ok(id)
    }

    /// Report the current status of a job as JSON.
    pub async fn status(&self, job_id: &str) -> Value {
        let jobs = self.jobs.read().await;
        let Some(entry) = jobs.get(job_id) else {
            return json!({"job_id": job_id, "status": "not_found"});
        };
        match &entry.state {
            JobState::Running { started_at } => json!({
                "job_id": job_id,
                "status": "running",
                "started_at": started_at,
                "hint": "The job is still running. Its result is pushed back to your task automatically when it finishes — no polling needed. Use the jobs tool to see all background jobs at once.",
            }),
            _ => render_status_json(job_id, &entry.state),
        }
    }

    /// Associate a job with its owning task. Called by the task executor
    /// after a background tool call so `cancel_for_task` can clean it up.
    ///
    /// Also closes a race: a short-lived job may finish (and call
    /// `mark_finished`/`mark_cancelled`) before this binding is recorded, in
    /// which case the completion notification carried `task_id: None` and was
    /// dropped by the consumer. If the job is already terminal here, re-fire
    /// the notification with the now-known task_id so the owning task still
    /// receives the result.
    pub async fn attach_task(&self, job_id: &str, task_id: &str) {
        let mut jobs = self.jobs.write().await;
        if let Some(entry) = jobs.get_mut(job_id) {
            entry.task_id = Some(task_id.to_string());
            if entry.state.is_terminal() {
                self.notify_completion(job_id, entry);
            }
        }
    }

    /// Cancel a single running job (kept for inspection afterwards).
    /// Returns false when the job does not exist or is not running.
    pub async fn cancel(&self, job_id: &str) -> bool {
        let mut jobs = self.jobs.write().await;
        let Some(entry) = jobs.get_mut(job_id) else {
            return false;
        };
        if !matches!(entry.state, JobState::Running { .. }) {
            return false;
        }
        if let Some(tx) = entry.kill.take() {
            let _ = tx.send(());
        }
        true
    }

    /// Cancel and drop every job owned by `task_id`. Called when a task
    /// ends, is removed, or is rolled back.
    pub async fn cancel_for_task(&self, task_id: &str) {
        let mut jobs = self.jobs.write().await;
        let ids: Vec<String> = jobs
            .iter()
            .filter(|(_, e)| e.task_id.as_deref() == Some(task_id))
            .map(|(id, _)| id.clone())
            .collect();
        for id in ids {
            if let Some(mut entry) = jobs.remove(&id)
                && let Some(tx) = entry.kill.take()
            {
                let _ = tx.send(());
            }
        }
    }

    async fn mark_finished(
        &self,
        id: &str,
        started_at: &str,
        shell: &str,
        command: &str,
        combined: String,
        success: bool,
        exit_code: Option<i32>,
        truncated: bool,
    ) {
        let mut jobs = self.jobs.write().await;
        let Some(entry) = jobs.get_mut(id) else {
            return;
        };
        entry.kill = None;
        let finished_at = chrono::Utc::now().to_rfc3339();
        entry.state = if success {
            JobState::Completed {
                output: combined.clone(),
                exit_code,
                truncated,
                // When the collected output was capped, the log file keeps
                // the full transcript for inspection.
                log_path: truncated
                    .then(|| write_output_log("job-logs", id, &combined).to_string_lossy().into_owned()),
                started_at: started_at.to_string(),
                finished_at,
            }
        } else {
            // The failure payload must not drown the model (or the user) in
            // progress-bar spam: `error` keeps the sanitized output for full
            // inspection, `error_reason` carries a short tail of the most
            // likely error lines plus a Windows-trap hint when one matches.
            // The full output always lands in a log file so the root cause
            // is recoverable even when the summary misses it.
            let diagnosed = append_windows_diagnostics(shell, command, &combined);
            JobState::Failed {
                error: combined.clone(),
                error_reason: summarize_error(&diagnosed, 1200),
                log_path: Some(
                    write_output_log("job-logs", id, &combined)
                        .to_string_lossy()
                        .into_owned(),
                ),
                exit_code,
                started_at: started_at.to_string(),
                finished_at,
            }
        };
        self.notify_completion(id, entry);
    }

    async fn mark_cancelled(&self, id: &str, started_at: &str) {
        let mut jobs = self.jobs.write().await;
        let Some(entry) = jobs.get_mut(id) else {
            return;
        };
        entry.kill = None;
        entry.state = JobState::Cancelled {
            started_at: started_at.to_string(),
            finished_at: chrono::Utc::now().to_rfc3339(),
        };
        self.notify_completion(id, entry);
    }
}

/// Render the terminal status JSON for a job (mirrors `status()` output for
/// completed/failed/cancelled states), used in completion notifications.
fn render_status_json(job_id: &str, state: &JobState) -> Value {
    match state {
        JobState::Completed {
            output,
            exit_code,
            truncated,
            log_path,
            started_at,
            finished_at,
        } => {
            let mut v = json!({
                "job_id": job_id,
                "status": "completed",
                "output": output,
                "started_at": started_at,
                "finished_at": finished_at,
            });
            if let Some(code) = exit_code {
                v["exit_code"] = json!(code);
            }
            if *truncated {
                v["truncated"] = json!(true);
            }
            if let Some(p) = log_path {
                v["log_path"] = json!(p);
            }
            v
        }
        JobState::Failed {
            error,
            error_reason,
            log_path,
            exit_code,
            started_at,
            finished_at,
        } => {
            let mut v = json!({
                "job_id": job_id,
                "status": "failed",
                "error": error,
                "error_reason": error_reason,
                "started_at": started_at,
                "finished_at": finished_at,
            });
            if let Some(code) = exit_code {
                v["exit_code"] = json!(code);
            }
            if let Some(p) = log_path {
                v["log_path"] = json!(p);
            }
            v
        }
        JobState::Cancelled {
            started_at,
            finished_at,
        } => json!({
            "job_id": job_id,
            "status": "cancelled",
            "started_at": started_at,
            "finished_at": finished_at,
        }),
        JobState::Running { .. } => json!({ "job_id": job_id, "status": "running" }),
    }
}

/// Strip PowerShell-specific noise from captured command output so the real
/// message survives instead of NativeCommandError formatting:
/// - pwsh 7 serializes native stderr as CLIXML (`#< CLIXML` + escape chars);
///   the message text inside `<S S="Error">...</S>` segments is extracted.
/// - Windows PowerShell 5.1 wraps error records with header lines
///   (`NativeCommandError`, `At line:`, `+ `, `~~~`, `+ CategoryInfo`,
///   `+ FullyQualifiedErrorId`) that add no information.
///
/// CRLF line endings (cmd.exe / native Windows tools) are normalized to LF
/// for every shell so downstream line-based processing and the model never
/// see stray `\r` characters. Lone `\r` (progress-redraw lines) is kept —
/// `summarize_error` relies on it to collapse progress bars.
///
/// Non-PowerShell output is returned unchanged apart from the line endings.
pub fn sanitize_shell_output(text: &str, shell: &str) -> String {
    let text = text.replace("\r\n", "\n");
    if shell != "powershell" {
        return text;
    }
    let text = if text.contains("#< CLIXML") {
        extract_clixml_messages(&text)
    } else {
        text
    };
    let mut out = Vec::with_capacity(text.len() / 32);
    for line in text.split('\n') {
        let trimmed = line.trim();
        if is_powershell_noise_line(trimmed) {
            continue;
        }
        out.push(line);
    }
    let joined = out.join("\n");
    joined.trim().to_string()
}

/// Pull the human-readable messages out of a pwsh 7 CLIXML stderr blob. Each
/// native stderr line arrives as `<S S="Error">text</S>`; non-matching content
/// falls back to the raw text (control chars removed).
fn extract_clixml_messages(text: &str) -> String {
    const TAG: &str = "<S S=\"Error\">";
    let mut out = String::new();
    let mut rest = text;
    while let Some(start) = rest.find(TAG) {
        let inner_start = start + TAG.len();
        let Some(end_rel) = rest[inner_start..].find("</S>") else {
            break;
        };
        let inner = &rest[inner_start..inner_start + end_rel];
        if !inner.trim().is_empty() {
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(inner);
        }
        rest = &rest[inner_start + end_rel + 4..];
    }
    if out.is_empty() {
        text.chars().filter(|c| !c.is_control()).collect::<String>()
    } else {
        out
    }
}

/// True when a trimmed PowerShell output line is error-record formatting that
/// should be dropped rather than surfaced to the model.
fn is_powershell_noise_line(trimmed: &str) -> bool {
    trimmed.is_empty()
        || trimmed == "NativeCommandError"
        || trimmed.starts_with("At line:")
        || trimmed == "+"
        || trimmed.starts_with("+ ")
        || trimmed.starts_with("+~")
        || trimmed.starts_with("~")
        || trimmed.starts_with("+ CategoryInfo")
        || trimmed.starts_with("+ FullyQualifiedErrorId")
        || trimmed.starts_with("CategoryInfo")
        || trimmed.starts_with("FullyQualifiedErrorId")
}

/// Condense a failed command's captured output into a short, readable reason:
/// progress-bar/spinner lines are dropped, only the last few meaningful lines
/// are kept, and the result is capped at `max_chars`. Used for the failed
/// job's `error_reason` and the foreground shell tool's error text, so a
/// multi-KB progress dump cannot drown the actual error (e.g. a 416 from a
/// failed download).
pub fn summarize_error(text: &str, max_chars: usize) -> String {
    let mut lines: Vec<&str> = Vec::new();
    for raw in text.split('\n') {
        let line = raw.trim_end_matches(['\r', '\n']);
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        // Progress/spinner lines: carriage-return redraws (mid-line `\r`),
        // bare percentages, and bar glyphs carry no diagnostic value. The
        // trailing `\r` of a Windows CRLF line is trimmed above and ignored.
        if line.contains('\r') {
            continue;
        }
        if trimmed.ends_with('%') && trimmed.len() < 16 {
            continue;
        }
        lines.push(line);
    }
    let start = lines.len().saturating_sub(12);
    let mut out = lines[start..].join("\n");
    let count = out.chars().count();
    if count > max_chars {
        let cutoff = out.floor_char_boundary(max_chars);
        out = format!("{}[... {} chars omitted]", &out[..cutoff], count - cutoff);
    }
    out
}

/// Append a short diagnostic hint when a failed command output matches a
/// common Windows trap (PowerShell aliases, execution policy, cmd-only
/// syntax, missing commands). Returns the original text plus hint lines so
/// the agent can fix the cause instead of blind-retrying.
#[cfg(windows)]
pub fn append_windows_diagnostics(shell: &str, command: &str, text: &str) -> String {
    let lower = text.to_lowercase();
    let cmd_lower = command.to_lowercase();
    let mut hints: Vec<&str> = Vec::new();

    if shell == "powershell" {
        // `curl` is an alias for Invoke-WebRequest in PowerShell; the user
        // almost always wants the real curl (curl.exe).
        if cmd_lower.contains("curl ") && !cmd_lower.contains("curl.exe") {
            hints.push(
                "PowerShell's `curl` is an alias for Invoke-WebRequest — use `curl.exe` (e.g. curl.exe -L <url>) for real curl behavior.",
            );
        }
        // `&&` is a parse error in PowerShell ("The token '&&' is not a valid
        // statement separator" / 标记“&&”不是此版本中的有效语句分隔符).
        if lower.contains("not a valid statement separator") || lower.contains("有效语句分隔符") {
            hints.push("`&&` is not valid in PowerShell — use `;` instead (or pass shell: cmd).");
        }
        // .ps1 scripts are blocked by the execution policy.
        if lower.contains("execution policy") || lower.contains("running scripts is disabled") {
            hints.push(
                "Windows script execution policy blocked a .ps1 — use the .cmd wrapper (e.g. npm.cmd instead of npm) or run: Set-ExecutionPolicy -Scope Process Bypass",
            );
        }
        if lower.contains("无法加载模块") || lower.contains("cannot load module") {
            hints.push(
                "A PowerShell module failed to load (e.g. Expand-Archive) — check the module path or install it with Install-Module.",
            );
        }
    } else {
        // cmd: Chinese Windows reports plain syntax errors with no detail.
        if lower.contains("语法不正确") {
            hints.push(
                "cmd reports a syntax error — check quoting and escaping: use double quotes around paths with spaces and escape & with ^ inside cmd.",
            );
        }
    }
    if lower.contains("禁止运行脚本") || lower.contains("执行策略") {
        hints.push(
            "Windows 脚本执行策略拦截了 .ps1 —— 改用 .cmd 包装（如 npm.cmd）或先执行 Set-ExecutionPolicy -Scope Process Bypass",
        );
    }
    if lower.contains("不是内部或外部命令") || lower.contains("not recognized as an internal or external command") {
        hints.push(
            "Command not found — check PATH or use the full path (e.g. C:\\Users\\<name>\\AppData\\Roaming\\npm\\npm.cmd).",
        );
    }
    if lower.contains("%1 不是有效的 win32") || lower.contains("not a valid win32 application") {
        hints.push(
            "'not a valid Win32 application' usually means a script without a launcher — invoke it via its interpreter (node/python) with the full script path, or use its .cmd wrapper.",
        );
    }
    if hints.is_empty() {
        text.to_string()
    } else {
        format!("{}\n\n[Windows trap?] {}", text, hints.join("\n"))
    }
}

#[cfg(not(windows))]
pub fn append_windows_diagnostics(_shell: &str, _command: &str, text: &str) -> String {
    text.to_string()
}

/// Terminate a child process together with its whole process tree. On
/// Windows, dropping a tokio Child only terminates the direct process; the
/// real command (a grandchild of the cmd.exe/powershell.exe wrapper) would
/// survive as an orphan otherwise.
async fn kill_process_tree(pid: u32) {
    #[cfg(windows)]
    {
        if let Ok(mut taskkill) = tokio::process::Command::new("taskkill")
            .args(["/T", "/F", "/PID", &pid.to_string()])
            .kill_on_drop(true)
            .spawn()
        {
            let _ = taskkill.wait().await;
        }
    }
    #[cfg(not(windows))]
    let _ = pid;
}

/// Read a child stdout/stderr stream into a String, capping at `max_bytes`
/// so runaway output cannot exhaust memory. After the cap is reached the
/// remaining bytes are still read and discarded: closing the pipe read end
/// early can make the child fail writes (broken pipe) and flip its exit code.
/// Returns `(text, overflowed)`.
pub(crate) async fn read_stream_capped<R>(stdout: Option<R>, max_bytes: usize) -> (String, bool)
where
    R: tokio::io::AsyncRead + Unpin,
{
    use tokio::io::AsyncReadExt;
    let Some(mut stream) = stdout else {
        return (String::new(), false);
    };
    let mut buf = Vec::with_capacity(max_bytes.min(8192));
    let mut tmp = [0u8; 8192];
    let mut overflowed = false;
    loop {
        match stream.read(&mut tmp).await {
            Ok(0) => break,
            Ok(n) => {
                let room = max_bytes.saturating_sub(buf.len());
                if room == 0 {
                    // Cap reached: keep draining until EOF so the child can
                    // finish writing normally; only the reported text is capped.
                    overflowed = true;
                    continue;
                }
                let take = n.min(room);
                buf.extend_from_slice(&tmp[..take]);
                if take < n {
                    overflowed = true;
                }
            }
            Err(_) => break,
        }
    }
    (haven_common::encoding::decode_lossy(&buf), overflowed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// Poll `status` until it is no longer "running" (or timeout).
    async fn wait_terminal(jobs: &BackgroundJobs, id: &str, timeout_secs: u64) -> Value {
        let deadline = std::time::Instant::now() + Duration::from_secs(timeout_secs);
        loop {
            let v = jobs.status(id).await;
            if v["status"] != "running" || std::time::Instant::now() > deadline {
                return v;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn test_completion_notified_on_finish() {
        let jobs = Arc::new(BackgroundJobs::new());
        let mut rx = jobs.take_completion_receiver().expect("receiver available");
        // Attach the task BEFORE the job finishes (normal path): the
        // completion must carry the task_id.
        let id = jobs
            .spawn_shell("echo done", "cmd", 20_000, None)
            .await
            .unwrap();
        jobs.attach_task(&id, "task-A").await;
        let v = wait_terminal(&jobs, &id, 10).await;
        assert_eq!(v["status"], "completed");
        let comp = tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .expect("completion received")
            .expect("channel open");
        assert_eq!(comp.job_id, id);
        assert_eq!(comp.status, "completed");
        assert_eq!(comp.task_id.as_deref(), Some("task-A"));
        assert!(
            comp.status_json["output"]
                .as_str()
                .unwrap()
                .contains("done")
        );
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn test_completion_refired_after_late_attach() {
        // Race path: the job finishes before attach_task is called. The
        // completion first fires with task_id=None; attach_task must re-fire
        // with the task_id so the owning task still gets notified.
        let jobs = Arc::new(BackgroundJobs::new());
        let mut rx = jobs.take_completion_receiver().expect("receiver available");
        let id = jobs
            .spawn_shell("echo fast", "cmd", 20_000, None)
            .await
            .unwrap();
        // Wait for the job to finish BEFORE attaching (simulate the race).
        let v = wait_terminal(&jobs, &id, 10).await;
        assert_eq!(v["status"], "completed");
        // Drain the task_id=None completion fired by mark_finished.
        let none_comp = rx.recv().await.expect("first completion");
        assert!(none_comp.task_id.is_none());
        // Now attach: should re-fire with the task_id.
        jobs.attach_task(&id, "task-B").await;
        let comp = tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .expect("refired completion received")
            .expect("channel open");
        assert_eq!(comp.task_id.as_deref(), Some("task-B"));
        assert_eq!(comp.status, "completed");
    }

    #[tokio::test]
    async fn test_completion_skipped_for_running() {
        let jobs = Arc::new(BackgroundJobs::new());
        // No jobs 鈫?no completion. Just confirm the receiver is taken.
        let _rx = jobs.take_completion_receiver().expect("receiver available");
        // status on not_found doesn't notify.
        assert_eq!(jobs.status("nope").await["status"], "not_found");
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn test_spawn_shell_completes_with_output() {
        let jobs = Arc::new(BackgroundJobs::new());
        let id = jobs
            .spawn_shell("echo bg-hello", "cmd", 20_000, None)
            .await
            .unwrap();
        let v = wait_terminal(&jobs, &id, 10).await;
        assert_eq!(v["status"], "completed", "got: {}", v);
        assert!(v["output"].as_str().unwrap().contains("bg-hello"));
        assert!(v["finished_at"].as_str().is_some());
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn test_spawn_shell_failure_reported() {
        let jobs = Arc::new(BackgroundJobs::new());
        let id = jobs
            .spawn_shell("exit 7", "cmd", 20_000, None)
            .await
            .unwrap();
        let v = wait_terminal(&jobs, &id, 10).await;
        assert_eq!(v["status"], "failed", "got: {}", v);
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn test_spawn_shell_stderr_captured() {
        let jobs = Arc::new(BackgroundJobs::new());
        let id = jobs
            .spawn_shell("echo err-msg 1>&2", "cmd", 20_000, None)
            .await
            .unwrap();
        let v = wait_terminal(&jobs, &id, 10).await;
        assert_eq!(v["status"], "completed", "got: {}", v);
        assert!(v["output"].as_str().unwrap().contains("err-msg"));
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn test_spawn_shell_cancelled() {
        let jobs = Arc::new(BackgroundJobs::new());
        let id = jobs
            .spawn_shell("ping -n 30 127.0.0.1", "cmd", 20_000, None)
            .await
            .unwrap();
        assert_eq!(jobs.status(&id).await["status"], "running");
        assert!(jobs.cancel(&id).await, "cancel must report success");
        let v = wait_terminal(&jobs, &id, 10).await;
        assert_eq!(v["status"], "cancelled", "got: {}", v);
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn test_cancel_for_task_cleans_up() {
        let jobs = Arc::new(BackgroundJobs::new());
        let id = jobs
            .spawn_shell("ping -n 30 127.0.0.1", "cmd", 20_000, None)
            .await
            .unwrap();
        jobs.attach_task(&id, "task-1").await;
        assert_eq!(jobs.status(&id).await["status"], "running");
        jobs.cancel_for_task("task-1").await;
        assert_eq!(jobs.status(&id).await["status"], "not_found");
    }

    #[tokio::test]
    async fn test_status_not_found() {
        let jobs = Arc::new(BackgroundJobs::new());
        assert_eq!(jobs.status("job-nope").await["status"], "not_found");
    }

    #[tokio::test]
    async fn test_cancel_unknown_job() {
        let jobs = Arc::new(BackgroundJobs::new());
        assert!(!jobs.cancel("job-nope").await);
    }

    #[tokio::test]
    async fn test_spawn_empty_command_rejected() {
        let jobs = Arc::new(BackgroundJobs::new());
        assert!(jobs.spawn_shell("  ", "cmd", 20_000, None).await.is_err());
    }

    #[tokio::test]
    async fn test_read_stream_capped_under_cap() {
        let (text, overflowed) = read_stream_capped(Some(&b"hello"[..]), 8192).await;
        assert_eq!(text, "hello");
        assert!(!overflowed);
    }

    #[tokio::test]
    async fn test_read_stream_capped_none() {
        let (text, overflowed) = read_stream_capped::<&[u8]>(None, 8192).await;
        assert_eq!(text, "");
        assert!(!overflowed);
    }

    #[tokio::test]
    async fn test_read_stream_capped_over_cap() {
        let data = vec![b'x'; 1000];
        let (text, overflowed) = read_stream_capped(Some(&data[..]), 100).await;
        assert_eq!(text.len(), 100);
        assert!(overflowed);
    }

    // ── sanitize_shell_output ─────────────────────────────────────────────

    #[test]
    fn test_sanitize_shell_output_normalizes_crlf() {
        let out = sanitize_shell_output("line1\r\nline2\r\n", "cmd");
        assert_eq!(out, "line1\nline2\n");
    }

    #[test]
    fn test_sanitize_shell_output_keeps_lone_cr_for_progress() {
        // Progress-redraw lines use lone `\r`; they must survive so
        // summarize_error can collapse them.
        let out = sanitize_shell_output("downloading 42%\rfinal", "cmd");
        assert!(out.contains('\r'), "lone \\r must be kept: {out:?}");
    }

    #[test]
    fn test_sanitize_shell_output_non_powershell_unchanged() {
        let text = "NativeCommandError\nreal line";
        assert_eq!(sanitize_shell_output(text, "cmd"), text);
    }

    #[test]
    fn test_sanitize_shell_output_strips_native_command_error_noise() {
        // Windows PowerShell 5.1 error-record formatting around a real error.
        let text = "NativeCommandError\ncurl.exe : curl: (7) Failed to connect\nAt line:1 char:1\n+ & curl.exe https://x\n+ ~~~~~~~~~~~~~~~~~~~~\n    + CategoryInfo          : NotSpecified: (curl.exe : ...)\n    + FullyQualifiedErrorId : NativeCommandError\n";
        let out = sanitize_shell_output(text, "powershell");
        assert!(!out.contains("NativeCommandError"), "got: {out}");
        assert!(!out.contains("CategoryInfo"), "got: {out}");
        assert!(!out.contains("At line:"), "got: {out}");
        assert!(
            out.contains("curl.exe : curl: (7) Failed to connect"),
            "real error must survive, got: {out}"
        );
    }

    #[test]
    fn test_sanitize_shell_output_strips_clixml_wrapper() {
        // pwsh 7 serializes native stderr as CLIXML.
        let text = format!(
            "#< CLIXML\n\u{1f}<Objs V=\"1\" S=\"Err\"><S S=\"Error\">boom: connection refused</S></Objs>"
        );
        let out = sanitize_shell_output(&text, "powershell");
        assert!(
            out.contains("boom: connection refused"),
            "message must survive CLIXML, got: {out}"
        );
        assert!(!out.contains("CLIXML"), "got: {out}");
    }

    #[test]
    fn test_sanitize_shell_output_keeps_plus_prefixed_content() {
        // A real `+line` (e.g. git diff addition) must not be mistaken for a
        // PowerShell continuation line.
        let out = sanitize_shell_output("+added line", "powershell");
        assert_eq!(out, "+added line");
    }

    // ── append_windows_diagnostics ────────────────────────────────────────

    #[cfg(windows)]
    #[test]
    fn test_windows_diagnostics_curl_alias() {
        let out = append_windows_diagnostics("powershell", "curl https://example.com", "curl.exe : Invoke-WebRequest failed");
        assert!(
            out.contains("Windows trap"),
            "a hint must be appended, got: {out}"
        );
        assert!(out.contains("curl.exe"), "hint must name curl.exe, got: {out}");
    }

    #[cfg(windows)]
    #[test]
    fn test_windows_diagnostics_no_hint_for_clean_error() {
        let out = append_windows_diagnostics("cmd", "echo hi", "some unrelated failure");
        assert_eq!(out, "some unrelated failure", "no hint, no change: {out}");
    }

    #[cfg(windows)]
    #[test]
    fn test_windows_diagnostics_cmd_syntax_error_cn() {
        let out = append_windows_diagnostics("cmd", "dir /q \\x", "该命令的语法不正确。");
        assert!(out.contains("Windows trap"), "got: {out}");
        assert!(out.contains("syntax"), "hint must explain quoting, got: {out}");
    }

    #[cfg(windows)]
    #[test]
    fn test_windows_diagnostics_powershell_and_chaining() {
        let out = append_windows_diagnostics(
            "powershell",
            "git clone x && cd y",
            "The token '&&' is not a valid statement separator in this version.",
        );
        assert!(out.contains("`;`"), "hint must suggest `;`, got: {out}");
    }

    #[cfg(windows)]
    #[test]
    fn test_windows_diagnostics_execution_policy() {
        let out = append_windows_diagnostics(
            "powershell",
            "npm install",
            "npm : 无法加载文件 npm.ps1，因为在此系统上禁止运行脚本",
        );
        assert!(out.contains("npm.cmd"), "hint must suggest the .cmd wrapper, got: {out}");
    }

    // ── summarize_error ───────────────────────────────────────────────────

    #[test]
    fn test_summarize_error_drops_progress_and_keeps_tail() {
        // Real progress bars redraw one line with mid-line carriage returns
        // (no newlines); the whole redraw must collapse to nothing while the
        // actual error line survives.
        let mut text = String::new();
        for i in 0..50 {
            text.push_str(&format!("[download] {i}% of 1000MiB in 00:0{i}\r"));
        }
        text.push_str("\ncurl: (7) Failed to connect to x port 443");
        let out = summarize_error(&text, 1200);
        assert!(
            !out.contains('%'),
            "progress lines must be dropped, got: {out}"
        );
        assert!(out.contains("Failed to connect"), "got: {out}");
    }

    #[test]
    fn test_summarize_error_caps_length() {
        let text = (0..200)
            .map(|i| format!("error line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let out = summarize_error(&text, 100);
        assert!(
            out.chars().count() <= 130,
            "got {} chars",
            out.chars().count()
        );
        assert!(out.contains("omitted"), "cap notice expected, got: {out}");
    }

    #[test]
    fn test_summarize_error_blank_lines_removed() {
        let out = summarize_error("a\n\n\nb", 1200);
        assert_eq!(out, "a\nb");
    }

    // ── list_for_task (jobs board) ────────────────────────────────────────

    #[cfg(windows)]
    #[tokio::test]
    async fn test_list_for_task_scopes_to_owning_task() {
        let jobs = Arc::new(BackgroundJobs::new());
        let id_a = jobs
            .spawn_shell("echo job-a", "cmd", 20_000, None)
            .await
            .unwrap();
        let id_b = jobs
            .spawn_shell("echo job-b", "cmd", 20_000, None)
            .await
            .unwrap();
        jobs.attach_task(&id_a, "task-1").await;
        jobs.attach_task(&id_b, "task-2").await;
        wait_terminal(&jobs, &id_a, 10).await;
        wait_terminal(&jobs, &id_b, 10).await;

        let rows = jobs.list_for_task("task-1").await;
        assert_eq!(rows.len(), 1, "only task-1's jobs: {rows:?}");
        assert_eq!(rows[0]["job_id"], id_a);
        assert_eq!(rows[0]["status"], "completed");
        assert!(
            rows[0]["preview"].as_str().unwrap().contains("job-a"),
            "preview expected, got: {rows:?}"
        );

        let all = jobs.list_for_task("task-2").await;
        assert_eq!(all.len(), 1);
        assert_eq!(all[0]["job_id"], id_b);
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn test_failed_job_reports_exit_code_and_reason() {
        let jobs = Arc::new(BackgroundJobs::new());
        let id = jobs
            .spawn_shell("echo progress... && exit 42", "cmd", 20_000, None)
            .await
            .unwrap();
        let v = wait_terminal(&jobs, &id, 10).await;
        assert_eq!(v["status"], "failed", "got: {v}");
        assert_eq!(v["exit_code"], 42, "exit code must be captured, got: {v}");
        assert!(
            v["error_reason"].as_str().is_some_and(|s| !s.is_empty()),
            "error_reason must be present, got: {v}"
        );
    }
}
