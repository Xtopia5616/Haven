use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{RwLock, mpsc, oneshot};
use tracing::Instrument;

use haven_memory::Database;
/// Maximum concurrent *running* background tasks per process. Prevents an
/// agent from leaking unbounded child processes. Finished tasks are reaped
/// on the next spawn, so this is a concurrency cap, not a lifetime cap.
/// Build the platform command used to run `command` in the requested
/// interpreter (cmd or powershell), with stdout/stderr piped. Window
/// suppression (`CREATE_NO_WINDOW`) is applied here unconditionally because
/// background tasks must never pop a console. The foreground `ShellTool`
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
            let ps = format!(
                "$OutputEncoding = [Console]::OutputEncoding = [System.Text.Encoding]::UTF8; \
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

/// Directory for per-command output logs (background tasks and failed
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
        tracing::warn!(task_id = %id, "failed to create output-log dir {}: {e}", dir.display());
        return path;
    }
    if let Err(e) = std::fs::write(&path, text) {
        tracing::warn!(task_id = %id, "failed to write output log {}: {e}", path.display());
    }
    path
}

/// Byte budget for collecting a command's combined stdout/stderr, derived
/// from the configured character cap (4 bytes/char worst case for UTF-8,
/// floored at 8 KiB). Shared by the foreground and background shell paths.
pub fn collect_byte_cap(max_chars: usize) -> usize {
    max_chars.saturating_mul(4).max(8192)
}

/// A background task that has reached a terminal state, surfaced to a consumer
/// (the agent layer) so the owning session can be auto-notified of the result
/// instead of the model having to poll `status`.
#[derive(Clone, Debug)]
pub struct BackgroundTaskCompletion {
    pub task_id: String,
    pub session_id: Option<String>,
    /// Terminal status string: "completed", "failed", or "cancelled".
    pub status: String,
    /// The task's status JSON (same shape `status()` returns for terminal
    /// states), carrying the output/error payload.
    pub status_json: Value,
}

/// Optional sink for background lifecycle events surfaced to the UI. The
/// sink is called with `(event, payload)` where event is one of:
/// - `task:created`  — a task was spawned `{ task_id, started_at }`
/// - `task:updated`  — the task was bound to a session `{ task_id, session_id }`
/// - `task:output`   — live output preview while the task runs
///   `{ task_id, output }` (bounded tail, emitted periodically)
/// - `task:finished` — the task reached a terminal state (full status
///   JSON, which already carries `task_id`, `status`, and the output/error
///   payload)
///
/// Shared by the scheduled-task registry (`task:created` / `task:finished`
/// / `task:updated`), which uses the same callback shape.
pub type EventSink = Arc<dyn Fn(String, serde_json::Value) + Send + Sync>;

/// Shared storage + forwarding for the UI event sink, used identically by
/// `BackgroundTasks` and the scheduled-task registry. Keeps the sink behind a
/// `Mutex<Option<_>>` so `set_event_sink` can be called once from the desktop
/// shell and `emit` is a no-op before that.
#[derive(Default)]
pub(crate) struct EventSinkState(Mutex<Option<EventSink>>);

impl EventSinkState {
    pub(crate) fn set(&self, sink: EventSink) {
        *self.0.lock().unwrap() = Some(sink);
    }

    pub(crate) fn emit(&self, event: &str, payload: Value) {
        if let Some(sink) = self.0.lock().unwrap().as_ref() {
            sink(event.to_string(), payload);
        }
    }
}

#[derive(Clone, Debug)]
enum BackgroundTaskState {
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

impl BackgroundTaskState {
    fn is_terminal(&self) -> bool {
        !matches!(self, BackgroundTaskState::Running { .. })
    }
}

struct BackgroundTask {
    session_id: Option<String>,
    state: BackgroundTaskState,
    /// Kill signal for the running child process.
    kill: Option<oneshot::Sender<()>>,
    /// Bounded tail of the combined live output, for `task:output` preview
    /// events while the task runs. `None` for terminal entries.
    tail: Option<Arc<Mutex<String>>>,
    /// The shell command this task is executing (surfaced in running status so
    /// the agent can see what the task is doing right now).
    command: String,
    /// Interpreter the command runs under ("cmd", "powershell", "bash", ...).
    shell: String,
}

/// True when a terminal entry has outlived the configured terminal-task TTL
/// (running entries are never stale). Entries with an unparseable
/// `finished_at` are kept (never wrongly reaped).
fn terminal_entry_stale(entry: &BackgroundTask, ttl: Duration) -> bool {
    let finished = match &entry.state {
        BackgroundTaskState::Completed { finished_at, .. }
        | BackgroundTaskState::Failed { finished_at, .. }
        | BackgroundTaskState::Cancelled { finished_at, .. } => finished_at,
        BackgroundTaskState::Running { .. } => return false,
    };
    let finished_ts = match chrono::DateTime::parse_from_rfc3339(finished) {
        Ok(t) => t.with_timezone(&chrono::Utc),
        Err(e) => {
            tracing::warn!(
                "terminal_entry_stale: unparseable finished_at '{}': {}",
                finished,
                e
            );
            return false;
        }
    };
    chrono::Utc::now() - finished_ts > chrono::Duration::from_std(ttl).unwrap()
}

/// Registry of background tool tasks (refine: long-running commands).
///
/// A task is spawned with `spawn_shell`, runs detached from the ReAct loop,
/// and is polled with `status`. Tasks are tied to a session via `attach_session`;
/// `cancel_for_session` kills and drops them when the session ends.
///
/// When a task finishes, a `BackgroundTaskCompletion` is sent on the completion channel
/// (see `take_completion_receiver`) so the agent layer can auto-inject the
/// result into the owning session's context without the model polling.
pub struct BackgroundTasks {
    tasks: RwLock<HashMap<String, BackgroundTask>>,
    completion_tx: mpsc::UnboundedSender<BackgroundTaskCompletion>,
    /// Receiver handed out exactly once to the consumer (the agent layer).
    completion_rx: Mutex<Option<mpsc::UnboundedReceiver<BackgroundTaskCompletion>>>,
    /// Max concurrent *running* tasks (from `context_limits.background_max_tasks`).
    max_tasks: RwLock<usize>,
    /// Live-output tail cap (chars) for `task:output` preview events (from
    /// `context_limits.background_job_tail_max_chars`).
    job_tail_max_chars: RwLock<usize>,
    /// Cadence of `task:output` events while a task produces output (from
    /// `context_limits.background_job_output_emit_interval_ms`).
    job_output_emit_interval: RwLock<Duration>,
    /// Terminal tasks stay on the board this long, then are reaped (from
    /// `context_limits.terminal_job_ttl_secs`).
    terminal_job_ttl: RwLock<Duration>,
    /// Optional UI event sink (see `EventSink`). Wired by the desktop shell
    /// to forward lifecycle events as Tauri events.
    event_sink: EventSinkState,
    /// Persistent store; `None` in headless/test builds (in-memory only).
    /// Terminal task rows stay here as history even after the in-memory board
    /// reaps them (`TERMINAL_JOB_TTL`), so results survive app restarts.
    db: RwLock<Option<Arc<Database>>>,
}

impl Default for BackgroundTasks {
    fn default() -> Self {
        Self::new()
    }
}

impl BackgroundTasks {
    pub fn new() -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        Self {
            tasks: RwLock::new(HashMap::new()),
            completion_tx: tx,
            completion_rx: Mutex::new(Some(rx)),
            max_tasks: RwLock::new(64),
            job_tail_max_chars: RwLock::new(2000),
            job_output_emit_interval: RwLock::new(Duration::from_millis(1500)),
            terminal_job_ttl: RwLock::new(Duration::from_secs(600)),
            event_sink: EventSinkState::default(),
            db: RwLock::new(None),
        }
    }

    /// Install the UI event sink (called once by the desktop shell).
    pub fn set_event_sink(&self, sink: EventSink) {
        self.event_sink.set(sink);
    }

    /// Forward a lifecycle event to the installed sink (no-op without one).
    fn emit(&self, event: &str, payload: Value) {
        self.event_sink.emit(event, payload);
    }

    /// Replace the unified context limits (background task concurrency cap,
    /// live-output tail size, output-event cadence, terminal-task TTL).
    pub async fn set_limits(&self, limits: &haven_common::config::ContextLimitsConfig) {
        *self.max_tasks.write().await = limits.background_max_tasks;
        *self.job_tail_max_chars.write().await = limits.background_job_tail_max_chars;
        *self.job_output_emit_interval.write().await =
            Duration::from_millis(limits.background_job_output_emit_interval_ms);
        *self.terminal_job_ttl.write().await = Duration::from_secs(limits.terminal_job_ttl_secs);
    }

    /// Attach the database used for persistence. Wired by the desktop shell
    /// (same handle the scheduled-task registry receives); headless tests skip it.
    pub async fn set_db(&self, db: Option<Arc<Database>>) {
        *self.db.write().await = db;
    }

    /// Post-restart cleanup: task rows a previous process left `running` are
    /// stale (their child processes died with the app), so mark them failed.
    /// Called once from the agent layer startup. Returns the number of rows
    /// marked. Idempotent.
    pub async fn restore_after_restart(&self) -> usize {
        let Some(db) = self.db.read().await.clone() else {
            return 0;
        };
        db.mark_interrupted_tasks().unwrap_or_else(|e| {
            tracing::warn!("restore_after_restart: failed to mark interrupted tasks: {e}");
            0
        })
    }

    /// Persist a terminal task row (its status payload + owning session) so the
    /// result survives the in-memory board's TTL and app restarts. No-op
    /// without a database. Must run outside the `tasks` lock is not required
    /// (the DB is a separate lock); callers may hold either.
    async fn persist_terminal(&self, task_id: &str, entry: &BackgroundTask) {
        let Some(db) = self.db.read().await.clone() else {
            return;
        };
        let (status, output, error, error_reason, log_path, exit_code, finished_at) =
            match &entry.state {
                BackgroundTaskState::Completed {
                    output,
                    exit_code,
                    log_path,
                    finished_at,
                    ..
                } => (
                    "completed",
                    Some(output.as_str()),
                    None,
                    None,
                    log_path.as_deref(),
                    *exit_code,
                    finished_at.as_str(),
                ),
                BackgroundTaskState::Failed {
                    error,
                    error_reason,
                    log_path,
                    exit_code,
                    finished_at,
                    ..
                } => (
                    "failed",
                    None,
                    Some(error.as_str()),
                    Some(error_reason.as_str()),
                    log_path.as_deref(),
                    *exit_code,
                    finished_at.as_str(),
                ),
                BackgroundTaskState::Cancelled { finished_at, .. } => (
                    "cancelled",
                    None,
                    None,
                    None,
                    None,
                    None,
                    finished_at.as_str(),
                ),
                BackgroundTaskState::Running { .. } => return,
            };
        if let Err(e) = db.finish_task(
            task_id,
            status,
            output,
            error,
            error_reason,
            log_path,
            exit_code,
            finished_at,
        ) {
            tracing::warn!(task_id = %task_id, "failed to persist task result: {e}");
        }
    }

    /// Take the completion receiver exactly once. The caller spawns a consumer
    /// loop that receives `BackgroundTaskCompletion`s and notifies the owning sessions.
    /// Returns `None` if already taken.
    pub fn take_completion_receiver(
        &self,
    ) -> Option<mpsc::UnboundedReceiver<BackgroundTaskCompletion>> {
        self.completion_rx.lock().unwrap().take()
    }

    /// Emit a completion notification for a task (if it has a terminal state),
    /// reading the owning session_id from the entry. Called from `mark_finished`,
    /// `mark_cancelled`, and `attach_session` (the latter to close the race where
    /// a task finishes before its session binding is recorded). Also persists the
    /// terminal row so the result survives restarts.
    async fn notify_completion(&self, task_id: &str, entry: &BackgroundTask) {
        if !entry.state.is_terminal() {
            return;
        }
        let status = match &entry.state {
            BackgroundTaskState::Completed { .. } => "completed",
            BackgroundTaskState::Failed { .. } => "failed",
            BackgroundTaskState::Cancelled { .. } => "cancelled",
            BackgroundTaskState::Running { .. } => return,
        };
        self.persist_terminal(task_id, entry).await;
        let status_json = render_status_json(task_id, &entry.state);
        self.emit("task:finished", status_json.clone());
        let _ = self.completion_tx.send(BackgroundTaskCompletion {
            task_id: task_id.to_string(),
            session_id: entry.session_id.clone(),
            status: status.to_string(),
            status_json,
        });
    }

    /// Board view of every task: one entry per task with status, timestamps,
    /// owning session id, and a bounded output/error preview. Surfaces the full
    /// task set to the UI (the per-session variant `list_for_session` serves the
    /// agent). Order: oldest first.
    pub async fn board(&self) -> Vec<Value> {
        let tasks = self.tasks.read().await;
        let mut rows = Vec::new();
        for (id, entry) in tasks.iter() {
            let mut row = match &entry.state {
                BackgroundTaskState::Running { .. } => running_status_json(id, entry),
                _ => render_status_json(id, &entry.state),
            };
            if let Some(tid) = &entry.session_id {
                row["session_id"] = json!(tid);
            }
            attach_preview(&mut row);
            rows.push(row);
        }
        rows.sort_by(|a, b| a["started_at"].as_str().cmp(&b["started_at"].as_str()));
        rows
    }

    /// Board view of every task owned by `session_id`: one entry per task with
    /// status, timestamps, and a bounded output/error preview. Lets the model
    /// see all background work of a session in a single call instead of polling
    /// `status` task by task. Order: oldest first.
    pub async fn list_for_session(&self, session_id: &str) -> Vec<Value> {
        let tasks = self.tasks.read().await;
        let mut rows = Vec::new();
        for (id, entry) in tasks.iter() {
            if entry.session_id.as_deref() != Some(session_id) {
                continue;
            }
            let mut row = match &entry.state {
                BackgroundTaskState::Running { .. } => running_status_json(id, entry),
                _ => render_status_json(id, &entry.state),
            };
            if let Some(tid) = &entry.session_id {
                row["session_id"] = json!(tid);
            }
            attach_preview(&mut row);
            rows.push(row);
        }
        rows.sort_by(|a, b| a["started_at"].as_str().cmp(&b["started_at"].as_str()));
        rows
    }

    /// Spawn a shell command as a background task. Returns the task id; the
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
        // Unpredictable task id: a sequential counter would let any
        // session's agent enumerate and read other sessions' background outputs
        // through status (which is RiskLevel::Safe).
        let id = haven_common::types::new_id("act");
        let started_at = chrono::Utc::now().to_rfc3339();
        let (kill_tx, kill_rx) = oneshot::channel();
        let tail = Arc::new(Mutex::new(String::new()));
        let tail_max_chars = *self.job_tail_max_chars.read().await;
        let emit_interval = *self.job_output_emit_interval.read().await;
        {
            let mut tasks = self.tasks.write().await;
            // Reap terminal entries first: their results were already
            // delivered via the completion channel, so they must not occupy
            // the cap forever (64 lifetime tasks would otherwise brick the
            // feature for long-lived sessions). Terminal entries older than
            // the configured terminal-task TTL are dropped the same way (the
            // UI panel and the persisted log files remain the record after
            // that).
            let terminal_ttl = *self.terminal_job_ttl.read().await;
            tasks.retain(|_, e| !terminal_entry_stale(e, terminal_ttl));
            let running = tasks
                .values()
                .filter(|e| matches!(e.state, BackgroundTaskState::Running { .. }))
                .count();
            if running >= *self.max_tasks.read().await {
                anyhow::bail!(
                    "too many running background tasks (limit {})",
                    *self.max_tasks.read().await
                );
            }
            tasks.insert(
                id.clone(),
                BackgroundTask {
                    session_id: None,
                    state: BackgroundTaskState::Running {
                        started_at: started_at.clone(),
                    },
                    kill: Some(kill_tx),
                    tail: Some(tail.clone()),
                    command: command.to_string(),
                    shell: shell.to_string(),
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
                // Spawn failed: remove the entry so the task is not left
                // dangling as "running".
                self.tasks.write().await.remove(&id);
                return Err(e.into());
            }
        };

        // Persist the spawn so task history survives restarts even when
        // the process dies mid-run (`restore_after_restart` marks such rows
        // failed). The session binding arrives later via `attach_session`.
        if let Some(db) = self.db.read().await.clone()
            && let Err(e) = db.save_task(&id, None, command, &started_at)
        {
            tracing::warn!(task_id = %id, "failed to persist task spawn: {e}");
        }

        let me = self.clone();
        let task_id = id.clone();
        let shell_owned = shell.to_string();
        let command_owned = command.to_string();
        self.emit(
            "task:created",
            json!({
                "task_id": task_id,
                "started_at": started_at,
            }),
        );
        // The direct child pid is captured before `run` moves `child`; on
        // Windows, cancelling must kill the whole process tree, not just the
        // cmd.exe/powershell.exe wrapper.
        let child_pid = child.id();
        // The task runner outlives its spawner: give it a task-level span so
        // every log line emitted while the task runs/cancels (output-log
        // writes, completion) carries the task id — parallel background tasks
        // stay distinguishable in logs.
        let task_span = tracing::info_span!("bg_task", task_id = %task_id);
        let runner_tail = tail.clone();
        let emit_task_id = task_id.clone();
        tokio::spawn(async move {
            // The task outlives this session: when `run` is dropped (kill signal
            // received), kill_on_drop terminates the child.
            let max_collect = collect_byte_cap(max_chars);
            let stdout_tail = runner_tail.clone();
            let stderr_tail = runner_tail.clone();
            let stdout_fut = read_stream_capped(
                child.stdout.take(),
                max_collect,
                Some(stdout_tail),
                tail_max_chars,
            );
            let stderr_fut = read_stream_capped(
                child.stderr.take(),
                max_collect,
                Some(stderr_tail),
                tail_max_chars,
            );
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
                    me.mark_cancelled(&task_id, &started_at).await;
                }
                (combined, success, exit_code, truncated) = &mut run => {
                    me.mark_finished(&task_id, &started_at, &shell_owned, &command_owned, combined, success, exit_code, truncated).await;
                }
            }
        }.instrument(task_span));

        // Live-output preview emitter: while the task runs, periodically push
        // the bounded tail of the combined stdout/stderr as `task:output`
        // events (only when it grew since the last tick). Stops as soon as
        // the entry leaves the Running state (finished, cancelled, or reaped).
        let emit_me = self.clone();
        let emit_tail = tail;
        tokio::spawn(async move {
            let mut last_len = 0usize;
            loop {
                tokio::time::sleep(emit_interval).await;
                if emit_me.status(&emit_task_id).await["status"].as_str() != Some("running") {
                    return;
                }
                let t = emit_tail.lock().unwrap();
                let len = t.len();
                if len != last_len {
                    last_len = len;
                    let output = t.clone();
                    drop(t);
                    emit_me.emit(
                        "task:output",
                        json!({ "task_id": emit_task_id, "output": output }),
                    );
                }
            }
        });

        Ok(id)
    }

    /// Report the current status of a task as JSON.
    pub async fn status(&self, task_id: &str) -> Value {
        let tasks = self.tasks.read().await;
        let Some(entry) = tasks.get(task_id) else {
            return json!({"task_id": task_id, "status": "not_found"});
        };
        match &entry.state {
            BackgroundTaskState::Running { .. } => {
                let mut v = running_status_json(task_id, entry);
                v["hint"] = json!(
                    "The task is still running. Its result is pushed back to your session automatically when it finishes — no polling needed. Use the tasks tool to see all background tasks at once."
                );
                v
            }
            _ => render_status_json(task_id, &entry.state),
        }
    }

    /// Associate a task with its owning session. Called by the session executor
    /// after a background tool call so `cancel_for_session` can clean it up.
    ///
    /// Also closes a race: a short-lived task may finish (and call
    /// `mark_finished`/`mark_cancelled`) before this binding is recorded, in
    /// which case the completion notification carried `session_id: None` and was
    /// dropped by the consumer. If the task is already terminal here, re-fire
    /// the notification with the now-known session_id so the owning session still
    /// receives the result.
    pub async fn attach_session(&self, task_id: &str, session_id: &str) {
        let mut tasks = self.tasks.write().await;
        if let Some(entry) = tasks.get_mut(task_id) {
            entry.session_id = Some(session_id.to_string());
            self.emit(
                "task:updated",
                json!({
                    "task_id": task_id,
                    "session_id": session_id,
                }),
            );
            // Record the owning session in the persisted row too, so terminal
            // history keeps its owner (spawn rows start with session_id NULL).
            if let Some(db) = self.db.read().await.clone()
                && let Err(e) = db.update_task_session(task_id, session_id)
            {
                tracing::warn!(task_id = %task_id, "failed to persist task session binding: {e}");
            }
            if entry.state.is_terminal() {
                self.notify_completion(task_id, entry).await;
            }
        }
    }

    /// Cancel a single running task (kept for inspection afterwards).
    /// Returns false when the task does not exist or is not running.
    pub async fn cancel(&self, task_id: &str) -> bool {
        let mut tasks = self.tasks.write().await;
        let Some(entry) = tasks.get_mut(task_id) else {
            return false;
        };
        if !matches!(entry.state, BackgroundTaskState::Running { .. }) {
            return false;
        }
        if let Some(tx) = entry.kill.take() {
            let _ = tx.send(());
        }
        true
    }

    /// Cancel and drop every task owned by `session_id`. Called when a session
    /// ends, is removed, or is rolled back.
    pub async fn cancel_for_session(&self, session_id: &str) {
        let mut tasks = self.tasks.write().await;
        let ids: Vec<String> = tasks
            .iter()
            .filter(|(_, e)| e.session_id.as_deref() == Some(session_id))
            .map(|(id, _)| id.clone())
            .collect();
        for id in ids {
            if let Some(mut entry) = tasks.remove(&id)
                && let Some(tx) = entry.kill.take()
            {
                let _ = tx.send(());
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
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
        let mut tasks = self.tasks.write().await;
        let Some(entry) = tasks.get_mut(id) else {
            return;
        };
        entry.kill = None;
        entry.tail = None;
        let finished_at = chrono::Utc::now().to_rfc3339();
        entry.state = if success {
            BackgroundTaskState::Completed {
                output: combined.clone(),
                exit_code,
                truncated,
                // When the collected output was capped, the log file keeps
                // the full transcript for inspection.
                log_path: truncated.then(|| {
                    write_output_log("task-logs", id, &combined)
                        .to_string_lossy()
                        .into_owned()
                }),
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
            BackgroundTaskState::Failed {
                error: combined.clone(),
                error_reason: summarize_error(&diagnosed, 1200),
                log_path: Some(
                    write_output_log("task-logs", id, &combined)
                        .to_string_lossy()
                        .into_owned(),
                ),
                exit_code,
                started_at: started_at.to_string(),
                finished_at,
            }
        };
        self.notify_completion(id, entry).await;
    }

    async fn mark_cancelled(&self, id: &str, started_at: &str) {
        let mut tasks = self.tasks.write().await;
        let Some(entry) = tasks.get_mut(id) else {
            return;
        };
        entry.kill = None;
        entry.tail = None;
        entry.state = BackgroundTaskState::Cancelled {
            started_at: started_at.to_string(),
            finished_at: chrono::Utc::now().to_rfc3339(),
        };
        self.notify_completion(id, entry).await;
    }
}

/// Attach a bounded `preview` (first 200 chars of output, else error) to a
/// status row. Shared by the board and scoped-list views.
fn attach_preview(row: &mut Value) {
    let preview = row
        .get("output")
        .and_then(|v| v.as_str())
        .or_else(|| row.get("error").and_then(|v| v.as_str()))
        .unwrap_or("");
    row["preview"] = json!(preview.chars().take(200).collect::<String>());
}

/// Render the running-state row for a task: the command line it is executing
/// and the bounded live-output tail, so the agent sees what the task is doing
/// right now instead of only "running". `output` is omitted while empty (the
/// command has not produced anything yet).
fn running_status_json(task_id: &str, entry: &BackgroundTask) -> Value {
    let mut v = json!({
        "task_id": task_id,
        "status": "running",
        "command": entry.command,
        "shell": entry.shell,
    });
    if let BackgroundTaskState::Running { started_at } = &entry.state {
        v["started_at"] = json!(started_at);
    }
    if let Some(tail) = &entry.tail {
        let out = tail.lock().unwrap();
        if !out.is_empty() {
            v["output"] = json!(out.as_str());
        }
    }
    v
}

/// Render the terminal status JSON for a task (mirrors `status()` output for
/// completed/failed/cancelled states), used in completion notifications.
fn render_status_json(task_id: &str, state: &BackgroundTaskState) -> Value {
    match state {
        BackgroundTaskState::Completed {
            output,
            exit_code,
            truncated,
            log_path,
            started_at,
            finished_at,
        } => {
            let mut v = json!({
                "task_id": task_id,
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
        BackgroundTaskState::Failed {
            error,
            error_reason,
            log_path,
            exit_code,
            started_at,
            finished_at,
        } => {
            let mut v = json!({
                "task_id": task_id,
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
        BackgroundTaskState::Cancelled {
            started_at,
            finished_at,
        } => json!({
            "task_id": task_id,
            "status": "cancelled",
            "started_at": started_at,
            "finished_at": finished_at,
        }),
        BackgroundTaskState::Running { .. } => json!({ "task_id": task_id, "status": "running" }),
    }
}

/// Strip PowerShell-specific noise from captured command output so the real
/// message survives instead of NativeCommandError formatting:
/// - pwsh 7 serializes native stderr as CLIXML (`#< CLIXML` + escape chars);
///   the message text inside `<S S="Error">...</S>` segments is extracted and
///   its XML entities (`&amp;`, `&gt;`) and `_xHHHH_` char escapes are decoded.
/// - Windows PowerShell 5.1 wraps error records with header lines
///   (`NativeCommandError`, `At line:`/`所在位置 行:`, `+ `, `~~~`,
///   `+ CategoryInfo`, `+ FullyQualifiedErrorId`) that add no information.
/// - pwsh 7 renders error records with `$PSStyle` ANSI colors even into a
///   pipe (`ESC[31;1m … ESC[0m`); those escapes are removed for every shell.
///
/// CRLF line endings (cmd.exe / native Windows tools) are normalized to LF
/// for every shell so downstream line-based processing and the model never
/// see stray `\r` characters. Lone `\r` (progress-redraw lines) is kept —
/// `summarize_error` relies on it to collapse progress bars.
///
/// Non-PowerShell output is returned unchanged apart from line-ending and
/// ANSI cleanup.
pub fn sanitize_shell_output(text: &str, shell: &str) -> String {
    let text = strip_ansi_escapes(text);
    let text = text.replace("\r\n", "\n");
    if shell != "powershell" && shell != "pwsh" {
        return text;
    }
    let text = if text.contains("#< CLIXML") {
        // ANSI escapes inside a CLIXML payload arrive as `_x001B_` and only
        // become literal ESC after unescaping, so strip again after extraction.
        strip_ansi_escapes(&replace_clixml_documents(&text))
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

/// Replace every CLIXML document (`#< CLIXML` … `</Objs>`) in `text` with the
/// human-readable messages inside it. Content before/after a document (e.g.
/// real stdout lines captured next to a CLIXML stderr blob) is preserved —
/// otherwise sanitizing would silently drop the actual command output.
fn replace_clixml_documents(text: &str) -> String {
    const HEADER: &str = "#< CLIXML";
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find(HEADER) {
        out.push_str(&rest[..start]);
        let doc_tail = &rest[start + HEADER.len()..];
        let Some(end_rel) = doc_tail.find("</Objs>") else {
            // Unterminated document (truncated capture): keep the remainder
            // as-is so no content is silently dropped.
            out.push_str(&rest[start..]);
            return out;
        };
        let doc_end = start + HEADER.len() + end_rel + "</Objs>".len();
        let doc = &rest[start..doc_end];
        let messages = extract_clixml_messages(doc);
        if !messages.is_empty() {
            if !out.is_empty() && !out.ends_with('\n') {
                out.push('\n');
            }
            out.push_str(&messages);
        }
        rest = &rest[doc_end..];
    }
    out.push_str(rest);
    out
}

/// Pull the human-readable messages out of a CLIXML stderr blob. Each native
/// stderr / error-record line arrives as `<S S="Error">text</S>`; the text is
/// XML-escaped (`&amp;`, `&gt;`) and PowerShell char-escaped (`_x000D_`,
/// `_x000A_` for CR/LF), so both are decoded and newlines normalized before
/// the messages are joined. Non-matching content falls back to the document's
/// text content (markup and control chars removed).
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
            out.push_str(&clixml_unescape(inner));
        }
        rest = &rest[inner_start + end_rel + 4..];
    }
    if out.is_empty() {
        let stripped: String = text.chars().filter(|c| !c.is_control()).collect::<String>();
        strip_xml_markup(&stripped)
    } else {
        out.replace("\r\n", "\n").replace('\r', "\n")
    }
}

/// Decode a CLIXML message payload: PowerShell `_xHHHH_` char escapes first
/// (`_x000D_` = CR, `_x000A_` = LF), then XML entities via the shared
/// `haven_common::encoding::xml_unescape` (`&amp;` last so a literal
/// `&amp;lt;` cannot be double-unescaped into `<`).
fn clixml_unescape(text: &str) -> String {
    let text = decode_x_escapes(text);
    haven_common::encoding::xml_unescape(&text)
}

/// Decode PowerShell's `_xHHHH_` character escapes (`_x000D_`, `_x000A_`,
/// `_x001B_`, …) to their code points. Any other text passes through.
fn decode_x_escapes(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '_'
            && i + 6 < chars.len()
            && chars[i + 1] == 'x'
            && chars[i + 2..i + 6].iter().all(|c| c.is_ascii_hexdigit())
            && chars[i + 6] == '_'
        {
            let hex: String = chars[i + 2..i + 6].iter().collect();
            if let Ok(v) = u32::from_str_radix(&hex, 16) {
                out.push(char::from_u32(v).unwrap_or('\u{FFFD}'));
                i += 7;
                continue;
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

/// Keep only the text content between XML tags (used when a CLIXML doc has no
/// `<S S="Error">` segments): skip markup and control characters.
fn strip_xml_markup(text: &str) -> String {
    let mut out = String::new();
    let mut in_tag = false;
    for c in text.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag && !c.is_control() => out.push(c),
            _ => {}
        }
    }
    out.trim().to_string()
}

/// Remove ANSI escape sequences (CSI, `ESC[…`) that pwsh 7 emits when it
/// renders error records with `$PSStyle` colors into a pipe.
fn strip_ansi_escapes(text: &str) -> String {
    // Most output has no escapes: return the input unchanged (memchr fast
    // path) instead of allocating a full-size copy on every shell command.
    if !text.contains('\u{1b}') {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' && chars.peek() == Some(&'[') {
            chars.next();
            // Parameter bytes (0x30–0x3F), then the final byte (0x40–0x7E).
            while let Some(&n) = chars.peek() {
                if ('\u{30}'..='\u{3f}').contains(&n) {
                    chars.next();
                } else {
                    break;
                }
            }
            if let Some(&n) = chars.peek()
                && ('\u{40}'..='\u{7e}').contains(&n)
            {
                chars.next();
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// True when a trimmed PowerShell output line is error-record formatting that
/// should be dropped rather than surfaced to the model.
fn is_powershell_noise_line(trimmed: &str) -> bool {
    trimmed.is_empty()
        || trimmed == "NativeCommandError"
        || trimmed.starts_with("At line:")
        // Localized position header (e.g. zh-CN: "所在位置 行:1 字符: 77").
        || trimmed.starts_with("所在位置")
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
/// task's `error_reason` and the foreground shell tool's error text, so a
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

    if shell == "powershell" || shell == "pwsh" {
        // `curl` is an alias for Invoke-WebRequest in PowerShell; the user
        // almost always wants the real curl (curl.exe).
        if cmd_lower.contains("curl ") && !cmd_lower.contains("curl.exe") {
            hints.push(
                "PowerShell's `curl` is an alias for Invoke-WebRequest — use `curl.exe` (e.g. curl.exe -L <url>) for real curl behavior.",
            );
        }
        // `&&` is a parse error in PowerShell ("The token '&&' is not a valid
        // statement separator" / 标记“&&”不是此版本中的有效语句分隔符).
        if lower.contains("not a valid statement separator") || lower.contains("有效语句分隔符")
        {
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
    if lower.contains("不是内部或外部命令")
        || lower.contains("not recognized as an internal or external command")
    {
        hints.push(
            "Command not found — check PATH or use the full path (e.g. C:\\Users\\<name>\\AppData\\Roaming\\npm\\npm.cmd).",
        );
    }
    if lower.contains("%1 不是有效的 win32") || lower.contains("not a valid win32 application")
    {
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

/// Append a decoded chunk to the shared live-output tail, keeping it bounded
/// to the last `max_chars` characters (dropping from the front). `max_chars`
/// comes from `context_limits.background_job_tail_max_chars`.
fn append_tail(tail: &Mutex<String>, chunk: &[u8], max_chars: usize) {
    let text = haven_common::encoding::decode_lossy(chunk);
    if text.is_empty() {
        return;
    }
    let mut t = tail.lock().unwrap();
    t.push_str(&text);
    while t.len() > max_chars {
        let overflow = t.len() - max_chars;
        let cut = t
            .char_indices()
            .nth(overflow)
            .map(|(i, _)| i)
            .unwrap_or(t.len());
        t.drain(..cut);
    }
}

/// Read a child stdout/stderr stream into a String, capping at `max_bytes`
/// so runaway output cannot exhaust memory. After the cap is reached the
/// remaining bytes are still read and discarded: closing the pipe read end
/// early can make the child fail writes (broken pipe) and flip its exit code.
/// When `tail` is given, every decoded chunk is also appended to the shared
/// bounded live-output tail (for `task:output` preview events).
/// Returns `(text, overflowed)`.
pub(crate) async fn read_stream_capped<R>(
    stdout: Option<R>,
    max_bytes: usize,
    tail: Option<Arc<Mutex<String>>>,
    tail_max_chars: usize,
) -> (String, bool)
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
    // Carry incomplete trailing UTF-8 bytes across read boundaries so a
    // multi-byte char split between two chunks is not mojibake'd in the live
    // tail preview. Non-UTF-8 streams (legacy GBK tools) are decoded lossily
    // per chunk and the carry is reset, so it never grows unbounded.
    let mut pending = Vec::new();
    loop {
        match stream.read(&mut tmp).await {
            Ok(0) => break,
            Ok(n) => {
                if let Some(t) = &tail {
                    pending.extend_from_slice(&tmp[..n]);
                    match std::str::from_utf8(&pending) {
                        Ok(s) => {
                            if !s.is_empty() {
                                append_tail(t, s.as_bytes(), tail_max_chars);
                            }
                            pending.clear();
                        }
                        Err(e) => {
                            let valid = e.valid_up_to();
                            if e.error_len().is_none() && pending.len() - valid <= 3 {
                                // Incomplete trailing sequence: flush the valid
                                // prefix and keep the remnant for the next read.
                                if valid > 0 {
                                    append_tail(t, &pending[..valid], tail_max_chars);
                                }
                                pending.drain(..valid);
                            } else {
                                // Not UTF-8 (e.g. GBK): decode the whole chunk
                                // lossily and reset the carry.
                                append_tail(t, &pending, tail_max_chars);
                                pending.clear();
                            }
                        }
                    }
                }
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
    async fn wait_terminal(tasks: &BackgroundTasks, id: &str, timeout_secs: u64) -> Value {
        let deadline = std::time::Instant::now() + Duration::from_secs(timeout_secs);
        loop {
            let v = tasks.status(id).await;
            if v["status"] != "running" || std::time::Instant::now() > deadline {
                return v;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    /// Spawn the two fixture echo tasks (`task-a` / `task-b`) and attach them to
    /// `ses-1` / `ses-2`. Shared by the board and scoped-list tests.
    async fn spawn_two_echo_tasks(tasks: &Arc<BackgroundTasks>) -> (String, String) {
        let id_a = tasks
            .spawn_shell("echo task-a", "cmd", 20_000, None)
            .await
            .unwrap();
        let id_b = tasks
            .spawn_shell("echo task-b", "cmd", 20_000, None)
            .await
            .unwrap();
        tasks.attach_session(&id_a, "ses-1").await;
        tasks.attach_session(&id_b, "ses-2").await;
        (id_a, id_b)
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn test_completion_notified_on_finish() {
        let tasks = Arc::new(BackgroundTasks::new());
        let mut rx = tasks
            .take_completion_receiver()
            .expect("receiver available");
        // Attach the session BEFORE the task finishes (normal path): the
        // completion must carry the session_id.
        let id = tasks
            .spawn_shell("echo done", "cmd", 20_000, None)
            .await
            .unwrap();
        tasks.attach_session(&id, "ses-A").await;
        let v = wait_terminal(&tasks, &id, 10).await;
        assert_eq!(v["status"], "completed");
        let comp = tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .expect("completion received")
            .expect("channel open");
        assert_eq!(comp.task_id, id);
        assert_eq!(comp.status, "completed");
        assert_eq!(comp.session_id.as_deref(), Some("ses-A"));
        assert!(
            comp.status_json["output"]
                .as_str()
                .unwrap()
                .contains("done")
        );
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn test_task_result_persisted_to_db() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let db = Arc::new(Database::open(&dir.path().join("test.db")).expect("temp db"));
        let tasks = Arc::new(BackgroundTasks::new());
        tasks.set_db(Some(db.clone())).await;

        let id = tasks
            .spawn_shell(
                "echo live-line & ping -n 4 127.0.0.1 >nul",
                "cmd",
                20_000,
                None,
            )
            .await
            .unwrap();
        tasks.attach_session(&id, "ses-DB").await;
        let v = wait_terminal(&tasks, &id, 10).await;
        assert_eq!(v["status"], "completed");

        // The status flips to completed before the terminal row is persisted
        // (mark_finished → notify_completion → persist_terminal); poll the
        // DB instead of reading it immediately.
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let row = loop {
            let rows = db.list_tasks(Some("background")).unwrap();
            if let Some(row) = rows.iter().find(|r| r.id == id) {
                break row.clone();
            }
            assert!(
                std::time::Instant::now() < deadline,
                "task row never persisted"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        };
        assert_eq!(row.kind, "background");
        assert_eq!(row.status.as_deref(), Some("completed"));
        assert_eq!(row.session_id.as_deref(), Some("ses-DB"));
        assert!(row.output.as_deref().unwrap().contains("live-line"));
        assert_eq!(row.exit_code, Some(0));
        assert!(row.finished_at.is_some());
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn test_completion_refired_after_late_attach() {
        // Race path: the task finishes before attach_session is called. The
        // completion first fires with session_id=None; attach_session must re-fire
        // with the session_id so the owning session still gets notified.
        let tasks = Arc::new(BackgroundTasks::new());
        let mut rx = tasks
            .take_completion_receiver()
            .expect("receiver available");
        let id = tasks
            .spawn_shell("echo fast", "cmd", 20_000, None)
            .await
            .unwrap();
        // Wait for the task to finish BEFORE attaching (simulate the race).
        let v = wait_terminal(&tasks, &id, 10).await;
        assert_eq!(v["status"], "completed");
        // Drain the session_id=None completion fired by mark_finished.
        let none_comp = rx.recv().await.expect("first completion");
        assert!(none_comp.session_id.is_none());
        // Now attach: should re-fire with the session_id.
        tasks.attach_session(&id, "ses-B").await;
        let comp = tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .expect("refired completion received")
            .expect("channel open");
        assert_eq!(comp.session_id.as_deref(), Some("ses-B"));
        assert_eq!(comp.status, "completed");
    }

    #[tokio::test]
    async fn test_completion_skipped_for_running() {
        let tasks = Arc::new(BackgroundTasks::new());
        // No tasks → no completion. Just confirm the receiver is taken.
        let _rx = tasks
            .take_completion_receiver()
            .expect("receiver available");
        // status on not_found doesn't notify.
        assert_eq!(tasks.status("nope").await["status"], "not_found");
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn test_spawn_shell_completes_with_output() {
        let tasks = Arc::new(BackgroundTasks::new());
        let id = tasks
            .spawn_shell("echo bg-hello", "cmd", 20_000, None)
            .await
            .unwrap();
        let v = wait_terminal(&tasks, &id, 10).await;
        assert_eq!(v["status"], "completed", "got: {}", v);
        assert!(v["output"].as_str().unwrap().contains("bg-hello"));
        assert!(v["finished_at"].as_str().is_some());
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn test_running_status_includes_command_and_live_output() {
        let tasks = Arc::new(BackgroundTasks::new());
        let id = tasks
            .spawn_shell(
                "echo live-line & ping -n 3 127.0.0.1 >nul",
                "cmd",
                20_000,
                None,
            )
            .await
            .unwrap();
        // While the task runs, status must carry the command line it executes.
        let v = tasks.status(&id).await;
        assert_eq!(v["status"], "running", "got: {}", v);
        assert_eq!(v["shell"], "cmd");
        assert!(
            v["command"].as_str().unwrap().contains("live-line"),
            "running status must include the command: {v}"
        );
        // And the live output tail once the command has produced something.
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            let v = tasks.status(&id).await;
            if v["output"].as_str().unwrap_or("").contains("live-line") {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "live output never arrived: {v}"
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        // The running row of the board carries the same command + output.
        let board = tasks.board().await;
        let row = board.iter().find(|r| r["task_id"] == id).expect("on board");
        assert!(row["command"].as_str().unwrap().contains("live-line"));
        assert!(row["preview"].as_str().unwrap_or("").contains("live-line"));
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn test_spawn_shell_failure_reported() {
        let tasks = Arc::new(BackgroundTasks::new());
        let id = tasks
            .spawn_shell("exit 7", "cmd", 20_000, None)
            .await
            .unwrap();
        let v = wait_terminal(&tasks, &id, 10).await;
        assert_eq!(v["status"], "failed", "got: {}", v);
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn test_spawn_shell_stderr_captured() {
        let tasks = Arc::new(BackgroundTasks::new());
        let id = tasks
            .spawn_shell("echo err-msg 1>&2", "cmd", 20_000, None)
            .await
            .unwrap();
        let v = wait_terminal(&tasks, &id, 10).await;
        assert_eq!(v["status"], "completed", "got: {}", v);
        assert!(v["output"].as_str().unwrap().contains("err-msg"));
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn test_spawn_shell_cancelled() {
        let tasks = Arc::new(BackgroundTasks::new());
        let id = tasks
            .spawn_shell("ping -n 30 127.0.0.1", "cmd", 20_000, None)
            .await
            .unwrap();
        assert_eq!(tasks.status(&id).await["status"], "running");
        assert!(tasks.cancel(&id).await, "cancel must report success");
        let v = wait_terminal(&tasks, &id, 10).await;
        assert_eq!(v["status"], "cancelled", "got: {}", v);
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn test_cancel_for_session_cleans_up() {
        let tasks = Arc::new(BackgroundTasks::new());
        let id = tasks
            .spawn_shell("ping -n 30 127.0.0.1", "cmd", 20_000, None)
            .await
            .unwrap();
        tasks.attach_session(&id, "ses-1").await;
        assert_eq!(tasks.status(&id).await["status"], "running");
        tasks.cancel_for_session("ses-1").await;
        assert_eq!(tasks.status(&id).await["status"], "not_found");
    }

    #[tokio::test]
    async fn test_status_not_found() {
        let tasks = Arc::new(BackgroundTasks::new());
        assert_eq!(tasks.status("task-nope").await["status"], "not_found");
    }

    #[tokio::test]
    async fn test_cancel_unknown_task() {
        let tasks = Arc::new(BackgroundTasks::new());
        assert!(!tasks.cancel("task-nope").await);
    }

    #[tokio::test]
    async fn test_spawn_empty_command_rejected() {
        let tasks = Arc::new(BackgroundTasks::new());
        assert!(tasks.spawn_shell("  ", "cmd", 20_000, None).await.is_err());
    }

    #[tokio::test]
    async fn test_read_stream_capped_under_cap() {
        let (text, overflowed) = read_stream_capped(Some(&b"hello"[..]), 8192, None, 2000).await;
        assert_eq!(text, "hello");
        assert!(!overflowed);
    }

    #[tokio::test]
    async fn test_read_stream_capped_none() {
        let (text, overflowed) = read_stream_capped::<&[u8]>(None, 8192, None, 2000).await;
        assert_eq!(text, "");
        assert!(!overflowed);
    }

    #[tokio::test]
    async fn test_read_stream_capped_over_cap() {
        let data = vec![b'x'; 1000];
        let (text, overflowed) = read_stream_capped(Some(&data[..]), 100, None, 2000).await;
        assert_eq!(text.len(), 100);
        assert!(overflowed);
    }

    #[tokio::test]
    async fn test_read_stream_capped_appends_tail() {
        let tail = Arc::new(Mutex::new(String::new()));
        let (text, _) =
            read_stream_capped(Some(&b"hello tail"[..]), 8192, Some(tail.clone()), 2000).await;
        assert_eq!(text, "hello tail");
        assert_eq!(*tail.lock().unwrap(), "hello tail");
        // A second chunk appends (multi-chunk tee).
        read_stream_capped(Some(&b" more"[..]), 8192, Some(tail.clone()), 2000).await;
        assert_eq!(*tail.lock().unwrap(), "hello tail more");
    }

    #[tokio::test]
    async fn test_read_stream_capped_tail_carries_split_multibyte() {
        // 8191 ASCII + a 3-byte UTF-8 char: the first 8192-byte read splits the
        // char (lead byte only), the second read finishes it. The live tail must
        // still show the char intact, not GBK-fallback mojibake.
        let tail = Arc::new(Mutex::new(String::new()));
        let mut content = "a".repeat(8191);
        content.push('中');
        read_stream_capped(Some(content.as_bytes()), 10_000, Some(tail.clone()), 2000).await;
        let t = tail.lock().unwrap();
        assert!(
            t.ends_with('中'),
            "tail must keep the split char intact, got: {:?}",
            &t[t.len().saturating_sub(40)..]
        );
        assert!(!t.contains('\u{FFFD}'), "no replacement chars in tail");
    }

    #[test]
    fn test_append_tail_bounded() {
        let tail = Mutex::new(String::new());
        // A single oversized chunk is truncated to the last max chars.
        let max_chars = 2000usize;
        let big = "x".repeat(max_chars + 500);
        append_tail(&tail, big.as_bytes(), max_chars);
        assert_eq!(tail.lock().unwrap().len(), max_chars);
        // Subsequent chunks drop the front.
        append_tail(&tail, "tail-end".as_bytes(), max_chars);
        let t = tail.lock().unwrap();
        assert!(
            t.ends_with("tail-end"),
            "got tail: {}",
            &t[t.len().saturating_sub(40)..]
        );
    }

    #[test]
    fn test_terminal_entry_stale_ttl() {
        let now = chrono::Utc::now();
        let entry = |finished: chrono::DateTime<chrono::Utc>, running: bool| BackgroundTask {
            session_id: None,
            state: if running {
                BackgroundTaskState::Running {
                    started_at: now.to_rfc3339(),
                }
            } else {
                BackgroundTaskState::Completed {
                    output: String::new(),
                    exit_code: None,
                    truncated: false,
                    log_path: None,
                    started_at: now.to_rfc3339(),
                    finished_at: finished.to_rfc3339(),
                }
            },
            kill: None,
            tail: None,
            command: String::new(),
            shell: "cmd".into(),
        };
        assert!(
            terminal_entry_stale(
                &entry(now - chrono::Duration::minutes(20), false),
                Duration::from_secs(600)
            ),
            "20-minute-old terminal task must be stale"
        );
        assert!(
            !terminal_entry_stale(
                &entry(now - chrono::Duration::minutes(5), false),
                Duration::from_secs(600)
            ),
            "fresh terminal task must be kept"
        );
        assert!(
            !terminal_entry_stale(&entry(now, true), Duration::from_secs(600)),
            "running task is never stale"
        );
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
        // Windows PowerShell 5.1 serializes a merged native error stream as a
        // CLIXML document on stderr. Payloads are XML-escaped (`&gt;`, `&amp;`)
        // and char-escaped (`_x000D_`/`_x000A_`), and the record carries
        // localized position/category noise lines that must be dropped.
        let text = concat!(
            "#< CLIXML\r\n",
            "<Objs Version=\"1.1.0.1\" xmlns=\"http://schemas.microsoft.com/powershell/2004/04\">",
            "<S S=\"Error\">cmd : boom: connection refused _x000D__x000A_</S>",
            "<S S=\"Error\">所在位置 行:1 字符: 77_x000D__x000A_</S>",
            "<S S=\"Error\">+ ... 2&gt;&amp;1_x000D__x000A_</S>",
            "<S S=\"Error\">+ ~~~~~~~~~~~~~~~~~~~~~_x000D__x000A_</S>",
            "<S S=\"Error\">    + CategoryInfo          : NotSpecified: (boom :String) [], RemoteException_x000D__x000A_</S>",
            "<S S=\"Error\">    + FullyQualifiedErrorId : NativeCommandError_x000D__x000A_</S>",
            "<S S=\"Error\"> _x000D__x000A_</S>",
            "</Objs>"
        );
        let out = sanitize_shell_output(text, "powershell");
        assert!(
            out.contains("boom: connection refused"),
            "message must survive CLIXML, got: {out}"
        );
        assert!(!out.contains("CLIXML"), "got: {out}");
        assert!(
            !out.contains("_x000D_"),
            "char escapes must be decoded, got: {out}"
        );
        assert!(
            !out.contains("&gt;") && !out.contains("&amp;"),
            "xml escapes must be decoded, got: {out}"
        );
        assert!(
            !out.contains("CategoryInfo"),
            "noise must be dropped, got: {out}"
        );
        assert!(
            !out.contains("所在位置"),
            "localized position line must be dropped, got: {out}"
        );
    }

    #[test]
    fn test_sanitize_shell_output_clixml_preserves_preceding_stdout() {
        // Real stdout captured next to a CLIXML stderr blob must not be lost:
        // only the CLIXML document is replaced, not the whole text.
        let text = "build succeeded\n#< CLIXML\n\u{1f}<Objs V=\"1\" S=\"Err\"><S S=\"Error\">boom: connection refused</S></Objs>";
        let out = sanitize_shell_output(text, "powershell");
        assert!(
            out.contains("build succeeded"),
            "stdout must survive, got: {out}"
        );
        assert!(out.contains("boom: connection refused"), "got: {out}");
        assert!(!out.contains("CLIXML"), "got: {out}");
    }

    #[test]
    fn test_sanitize_shell_output_strips_ansi_escapes() {
        // pwsh 7 renders error records with $PSStyle ANSI colors even into a
        // pipe; the escapes must not reach the model.
        let out = sanitize_shell_output("\u{1b}[31;1mboom\u{1b}[0m", "powershell");
        assert_eq!(out, "boom");
        let out = sanitize_shell_output("\u{1b}[31;1mboom\u{1b}[0m", "cmd");
        assert_eq!(out, "boom");
    }

    #[test]
    fn test_sanitize_shell_output_strips_ansi_escaped_inside_clixml() {
        // Inside a CLIXML payload the ESC byte is char-escaped as `_x001B_`;
        // the escape must be stripped after unescaping, not just on the raw
        // text (where no literal ESC exists yet).
        let text = concat!(
            "#< CLIXML\r\n",
            "<Objs V=\"1\" S=\"Err\">",
            "<S S=\"Error\">_x001B_[31;1mboom: refused_x001B_[0m _x000D__x000A_</S>",
            "</Objs>"
        );
        let out = sanitize_shell_output(text, "powershell");
        assert_eq!(out, "boom: refused", "got: {out:?}");
    }

    #[test]
    fn test_sanitize_shell_output_non_powershell_still_strips_ansi() {
        let out = sanitize_shell_output("\u{1b}[2K\r34%", "cmd");
        assert_eq!(out, "\r34%", "non-ANSI control must be untouched");
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
        let out = append_windows_diagnostics(
            "powershell",
            "curl https://example.com",
            "curl.exe : Invoke-WebRequest failed",
        );
        assert!(
            out.contains("Windows trap"),
            "a hint must be appended, got: {out}"
        );
        assert!(
            out.contains("curl.exe"),
            "hint must name curl.exe, got: {out}"
        );
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
        assert!(
            out.contains("syntax"),
            "hint must explain quoting, got: {out}"
        );
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
        assert!(
            out.contains("npm.cmd"),
            "hint must suggest the .cmd wrapper, got: {out}"
        );
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

    // ── list_for_session (tasks board) ────────────────────────────────────────

    #[cfg(windows)]
    #[tokio::test]
    async fn test_event_sink_receives_lifecycle() {
        let tasks = Arc::new(BackgroundTasks::new());
        let events = Arc::new(std::sync::Mutex::new(Vec::new()));
        let sink_events = events.clone();
        tasks.set_event_sink(Arc::new(move |name, payload| {
            sink_events.lock().unwrap().push((name, payload));
        }));
        let id = tasks
            .spawn_shell("echo bg-event", "cmd", 20_000, None)
            .await
            .unwrap();
        tasks.attach_session(&id, "ses-evt").await;
        wait_terminal(&tasks, &id, 10).await;

        let evs = events.lock().unwrap();
        let names: Vec<&str> = evs.iter().map(|(n, _)| n.as_str()).collect();
        assert!(names.contains(&"task:created"), "got: {names:?}");
        assert!(names.contains(&"task:updated"), "got: {names:?}");
        assert!(names.contains(&"task:finished"), "got: {names:?}");
        let term = evs
            .iter()
            .find(|(n, _)| n == "task:finished")
            .expect("terminal event");
        assert_eq!(term.1["status"], "completed");
        assert_eq!(term.1["task_id"], id);
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn test_board_lists_all_jobs_with_session() {
        let tasks = Arc::new(BackgroundTasks::new());
        let (id_a, id_b) = spawn_two_echo_tasks(&tasks).await;
        wait_terminal(&tasks, &id_a, 10).await;
        wait_terminal(&tasks, &id_b, 10).await;

        let rows = tasks.board().await;
        assert_eq!(rows.len(), 2, "all tasks on board: {rows:?}");
        let by_id: HashMap<_, _> = rows
            .iter()
            .map(|r| (r["task_id"].as_str().unwrap(), r))
            .collect();
        assert_eq!(by_id[&id_a.as_str()]["session_id"], "ses-1");
        assert_eq!(by_id[&id_b.as_str()]["session_id"], "ses-2");
        assert_eq!(by_id[&id_a.as_str()]["status"], "completed");
        assert!(
            by_id[&id_a.as_str()]["preview"]
                .as_str()
                .unwrap()
                .contains("task-a"),
            "preview expected, got: {rows:?}"
        );
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn test_task_output_preview_emitted() {
        let tasks = Arc::new(BackgroundTasks::new());
        let events = Arc::new(std::sync::Mutex::new(Vec::new()));
        let sink_events = events.clone();
        tasks.set_event_sink(Arc::new(move |name, payload| {
            sink_events.lock().unwrap().push((name, payload));
        }));
        // A task that keeps running past one emit interval while producing
        // output (ping lasts ~3s), so the preview event has time to fire.
        let id = tasks
            .spawn_shell(
                "echo preview-line-123 && ping -n 4 127.0.0.1 > nul",
                "cmd",
                20_000,
                None,
            )
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(2500)).await;
        {
            let evs = events.lock().unwrap();
            let output_evt = evs
                .iter()
                .find(|(n, _)| n == "task:output")
                .expect("task:output event must be emitted while running");
            assert_eq!(output_evt.1["task_id"], id);
            assert!(
                output_evt.1["output"]
                    .as_str()
                    .unwrap()
                    .contains("preview-line-123"),
                "preview must carry the echoed line, got: {:?}",
                output_evt.1["output"]
            );
        }
        let _ = tasks.cancel(&id).await;
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn test_list_for_session_scopes_to_owning_session() {
        let tasks = Arc::new(BackgroundTasks::new());
        let (id_a, id_b) = spawn_two_echo_tasks(&tasks).await;
        wait_terminal(&tasks, &id_a, 10).await;
        wait_terminal(&tasks, &id_b, 10).await;

        let rows = tasks.list_for_session("ses-1").await;
        assert_eq!(rows.len(), 1, "only ses-1's tasks: {rows:?}");
        assert_eq!(rows[0]["task_id"], id_a);
        assert_eq!(rows[0]["status"], "completed");
        assert!(
            rows[0]["preview"].as_str().unwrap().contains("task-a"),
            "preview expected, got: {rows:?}"
        );

        let all = tasks.list_for_session("ses-2").await;
        assert_eq!(all.len(), 1);
        assert_eq!(all[0]["task_id"], id_b);
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn test_failed_task_reports_exit_code_and_reason() {
        let tasks = Arc::new(BackgroundTasks::new());
        let id = tasks
            .spawn_shell("echo progress... && exit 42", "cmd", 20_000, None)
            .await
            .unwrap();
        let v = wait_terminal(&tasks, &id, 10).await;
        assert_eq!(v["status"], "failed", "got: {v}");
        assert_eq!(v["exit_code"], 42, "exit code must be captured, got: {v}");
        assert!(
            v["error_reason"].as_str().is_some_and(|s| !s.is_empty()),
            "error_reason must be present, got: {v}"
        );
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
        assert!(payload.ends_with("Write-Output hi"));
    }
}
