use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;
use tokio::sync::{RwLock, mpsc, oneshot};
use tracing::Instrument;

fn lock_or_recover<'a, T>(lock: &'a Mutex<T>, name: &'static str) -> MutexGuard<'a, T> {
    lock.lock().unwrap_or_else(|poisoned| {
        tracing::error!(
            lock = name,
            "background action lock poisoned; recovering state"
        );
        poisoned.into_inner()
    })
}

use crate::output::{append_windows_diagnostics, sanitize_shell_output, summarize_error};
use crate::process::{kill_process_tree, read_stream_capped, take_tail_if_changed};
use crate::shell_runtime::{build_shell_command, collect_byte_cap, write_output_log};
use haven_memory::Database;

/// A background action that has reached a terminal state, surfaced to a consumer
/// (the agent layer) so the owning session can be auto-notified of the result
/// instead of the model having to poll `status`.
#[derive(Clone, Debug)]
pub struct BackgroundActionCompletion {
    pub action_id: String,
    pub session_id: Option<String>,
    /// Terminal status string: "completed", "failed", or "cancelled".
    pub status: String,
    /// The action's status JSON (same shape `status()` returns for terminal
    /// states), carrying the output/error payload.
    pub status_json: Value,
}

/// Optional sink for background lifecycle events surfaced to the UI. The
/// sink is called with `(event, payload)` where event is one of:
/// - `action:created`  — a action was spawned
///   `{ action_id, status: "running", kind: "background", started_at }`
/// - `action:updated`  — the action was bound to a session `{ action_id, session_id }`
/// - `action:output`   — live output preview while the action runs
///   `{ action_id, status: "running", output }` (bounded tail, emitted periodically)
/// - `action:finished` — the action reached a terminal state (full status
///   JSON, which already carries `action_id`, `status`, and the output/error
///   payload)
///
/// Shared by the scheduled-action registry (`action:created` / `action:finished`
/// / `action:updated`), which uses the same callback shape.
pub type EventSink = Arc<dyn Fn(String, serde_json::Value) + Send + Sync>;

/// Shared storage + forwarding for the UI event sink, used identically by
/// `BackgroundActions` and the scheduled-action registry. Keeps the sink behind a
/// `Mutex<Option<_>>` so `set_event_sink` can be called once from the desktop
/// shell and `emit` is a no-op before that.
#[derive(Default)]
pub(crate) struct EventSinkState(Mutex<Option<EventSink>>);

impl EventSinkState {
    pub(crate) fn set(&self, sink: EventSink) {
        *lock_or_recover(&self.0, "event_sink") = Some(sink);
    }

    pub(crate) fn emit(&self, event: &str, payload: Value) {
        if let Some(sink) = lock_or_recover(&self.0, "event_sink").as_ref() {
            sink(event.to_string(), payload);
        }
    }
}

#[derive(Clone, Debug)]
enum BackgroundActionState {
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

impl BackgroundActionState {
    fn is_terminal(&self) -> bool {
        !matches!(self, BackgroundActionState::Running { .. })
    }
}

struct BackgroundAction {
    session_id: Option<String>,
    state: BackgroundActionState,
    /// Kill signal for the running child process.
    kill: Option<oneshot::Sender<()>>,
    /// Bounded tail of the combined live output, for `action:output` preview
    /// events while the action runs. `None` for terminal entries.
    tail: Option<Arc<Mutex<String>>>,
    /// The shell command this action is executing (surfaced in running status so
    /// the agent can see what the action is doing right now).
    command: String,
    /// Interpreter the command runs under ("cmd", "powershell", "bash", ...).
    shell: String,
}

/// True when a terminal entry has outlived the configured terminal-action TTL
/// (running entries are never stale). Entries with an unparseable
/// `finished_at` are kept (never wrongly reaped).
fn terminal_entry_stale(entry: &BackgroundAction, ttl: Duration) -> bool {
    let finished = match &entry.state {
        BackgroundActionState::Completed { finished_at, .. }
        | BackgroundActionState::Failed { finished_at, .. }
        | BackgroundActionState::Cancelled { finished_at, .. } => finished_at,
        BackgroundActionState::Running { .. } => return false,
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
    let Ok(ttl) = chrono::Duration::from_std(ttl) else {
        // An unrepresentable duration is effectively infinite from the
        // action registry's perspective; retain the entry rather than panic
        // during cleanup.
        return false;
    };
    chrono::Utc::now() - finished_ts > ttl
}

/// Registry of background tool actions (refine: long-running commands).
///
/// A action is spawned with `spawn_shell`, runs detached from the ReAct loop,
/// and is polled with `status`. Actions are tied to a session via `attach_session`;
/// `cancel_for_session` kills and drops them when the session ends.
///
/// When a action finishes, a `BackgroundActionCompletion` is sent on the completion channel
/// (see `take_completion_receiver`) so the agent layer can auto-inject the
/// result into the owning session's context without the model polling.
pub struct BackgroundActions {
    actions: RwLock<HashMap<String, BackgroundAction>>,
    /// Serializes spawn admission and durable registration. An action is not
    /// visible to cancellation until its `running` row is durable, avoiding
    /// orphaned DB rows or processes across the spawn failure window.
    spawn_gate: tokio::sync::Mutex<()>,
    completion_tx: mpsc::UnboundedSender<BackgroundActionCompletion>,
    /// Receiver handed out exactly once to the consumer (the agent layer).
    completion_rx: Mutex<Option<mpsc::UnboundedReceiver<BackgroundActionCompletion>>>,
    /// Max concurrent *running* actions (from `context_limits.background_max_actions`).
    max_actions: RwLock<usize>,
    /// Live-output tail cap (chars) for `action:output` preview events (from
    /// `context_limits.background_job_tail_max_chars`).
    job_tail_max_chars: RwLock<usize>,
    /// Cadence of `action:output` events while a action produces output (from
    /// `context_limits.background_job_output_emit_interval_ms`).
    job_output_emit_interval: RwLock<Duration>,
    /// Terminal actions stay on the board this long, then are reaped (from
    /// `context_limits.terminal_job_ttl_secs`).
    terminal_job_ttl: RwLock<Duration>,
    /// Optional UI event sink (see `EventSink`). Wired by the desktop shell
    /// to forward lifecycle events as Tauri events.
    event_sink: EventSinkState,
    /// Persistent store; `None` in headless/test builds (in-memory only).
    /// Terminal action rows stay here as history even after the in-memory board
    /// reaps them (`TERMINAL_JOB_TTL`), so results survive app restarts.
    db: RwLock<Option<Arc<Database>>>,
}

impl Default for BackgroundActions {
    fn default() -> Self {
        Self::new()
    }
}

impl BackgroundActions {
    pub fn new() -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        Self {
            actions: RwLock::new(HashMap::new()),
            spawn_gate: tokio::sync::Mutex::new(()),
            completion_tx: tx,
            completion_rx: Mutex::new(Some(rx)),
            max_actions: RwLock::new(64),
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

    /// Replace the unified context limits (background action concurrency cap,
    /// live-output tail size, output-event cadence, terminal-action TTL).
    pub async fn set_limits(&self, limits: &haven_common::config::ContextLimitsConfig) {
        *self.max_actions.write().await = limits.background_max_actions;
        *self.job_tail_max_chars.write().await = limits.background_job_tail_max_chars;
        *self.job_output_emit_interval.write().await =
            Duration::from_millis(limits.background_job_output_emit_interval_ms);
        *self.terminal_job_ttl.write().await = Duration::from_secs(limits.terminal_job_ttl_secs);
    }

    /// Attach the database used for persistence. Wired by the desktop shell
    /// (same handle the scheduled-action registry receives); headless tests skip it.
    pub async fn set_db(&self, db: Option<Arc<Database>>) {
        *self.db.write().await = db;
    }

    /// Post-restart cleanup: action rows a previous process left `running` are
    /// stale (their child processes died with the app), so mark them failed.
    /// Called once from the agent layer startup. Returns the number of rows
    /// marked. Idempotent.
    pub async fn restore_after_restart(&self) -> usize {
        let Some(db) = self.db.read().await.clone() else {
            return 0;
        };
        db.run_blocking(|db| db.mark_interrupted_actions())
            .await
            .unwrap_or_else(|e| {
                tracing::warn!("restore_after_restart: failed to mark interrupted actions: {e}");
                0
            })
    }

    /// Persist a terminal action row (its status payload + owning session) so the
    /// result survives the in-memory board's TTL and app restarts. No-op
    /// without a database. Must run outside the `actions` lock is not required
    /// (the DB is a separate lock); callers may hold either.
    async fn persist_terminal(&self, action_id: &str, state: &BackgroundActionState) {
        let Some(db) = self.db.read().await.clone() else {
            return;
        };
        let (status, output, error, error_reason, log_path, exit_code, finished_at) = match state {
            BackgroundActionState::Completed {
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
            BackgroundActionState::Failed {
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
            BackgroundActionState::Cancelled { finished_at, .. } => (
                "cancelled",
                None,
                None,
                None,
                None,
                None,
                finished_at.as_str(),
            ),
            BackgroundActionState::Running { .. } => return,
        };
        let action_id = action_id.to_string();
        let status = status.to_string();
        let output = output.map(str::to_string);
        let error = error.map(str::to_string);
        let error_reason = error_reason.map(str::to_string);
        let log_path = log_path.map(str::to_string);
        let finished_at = finished_at.to_string();
        let action_id_for_db = action_id.clone();
        if let Err(e) = db
            .run_blocking(move |db| {
                db.finish_action(
                    &action_id_for_db,
                    &status,
                    output.as_deref(),
                    error.as_deref(),
                    error_reason.as_deref(),
                    log_path.as_deref(),
                    exit_code,
                    &finished_at,
                )
            })
            .await
        {
            tracing::warn!(action_id = %action_id, "failed to persist action result: {e}");
        }
    }

    /// Take the completion receiver exactly once. The caller spawns a consumer
    /// loop that receives `BackgroundActionCompletion`s and notifies the owning sessions.
    /// Returns `None` if already taken.
    pub fn take_completion_receiver(
        &self,
    ) -> Option<mpsc::UnboundedReceiver<BackgroundActionCompletion>> {
        lock_or_recover(&self.completion_rx, "completion_receiver").take()
    }

    /// Emit a completion notification for a action (if it has a terminal state),
    /// reading the owning session_id from the entry. Called from `mark_finished`,
    /// `mark_cancelled`, and `attach_session` (the latter to close the race where
    /// a action finishes before its session binding is recorded). Also persists the
    /// terminal row so the result survives restarts.
    async fn notify_completion(
        &self,
        action_id: &str,
        state: BackgroundActionState,
        session_id: Option<String>,
    ) {
        if !state.is_terminal() {
            return;
        }
        let status = match &state {
            BackgroundActionState::Completed { .. } => "completed",
            BackgroundActionState::Failed { .. } => "failed",
            BackgroundActionState::Cancelled { .. } => "cancelled",
            BackgroundActionState::Running { .. } => return,
        };
        self.persist_terminal(action_id, &state).await;
        let status_json = render_status_json(action_id, &state);
        self.emit("action:finished", status_json.clone());
        let _ = self.completion_tx.send(BackgroundActionCompletion {
            action_id: action_id.to_string(),
            session_id,
            status: status.to_string(),
            status_json,
        });
    }

    /// Board view of every action: one entry per action with status, timestamps,
    /// owning session id, and a bounded output/error preview. Surfaces the full
    /// action set to the UI (the per-session variant `list_for_session` serves the
    /// agent). Order: oldest first.
    pub async fn board(&self) -> Vec<Value> {
        let actions = self.actions.read().await;
        let mut rows = Vec::new();
        for (id, entry) in actions.iter() {
            let mut row = match &entry.state {
                BackgroundActionState::Running { .. } => running_status_json(id, entry),
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

    /// Board view of every action owned by `session_id`: one entry per action with
    /// status, timestamps, and a bounded output/error preview. Lets the model
    /// see all background work of a session in a single call instead of polling
    /// `status` action by action. Order: oldest first.
    pub async fn list_for_session(&self, session_id: &str) -> Vec<Value> {
        let actions = self.actions.read().await;
        let mut rows = Vec::new();
        for (id, entry) in actions.iter() {
            if entry.session_id.as_deref() != Some(session_id) {
                continue;
            }
            let mut row = match &entry.state {
                BackgroundActionState::Running { .. } => running_status_json(id, entry),
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

    /// Spawn a shell command as a background action. Returns the action id; the
    /// command keeps running after this function returns. `cwd` overrides the
    /// default Temp working directory when provided.
    pub async fn spawn_shell(
        self: &Arc<Self>,
        command: &str,
        shell: &str,
        max_chars: usize,
        cwd: Option<std::path::PathBuf>,
    ) -> anyhow::Result<String> {
        self.spawn_shell_for_session(command, shell, max_chars, cwd, None)
            .await
    }

    /// Spawn a background action with its owner bound before the process is
    /// published. Agent calls should use this variant so session shutdown can
    /// cancel a process even if it exits during the tool-result projection.
    pub async fn spawn_shell_for_session(
        self: &Arc<Self>,
        command: &str,
        shell: &str,
        max_chars: usize,
        cwd: Option<std::path::PathBuf>,
        session_id: Option<&str>,
    ) -> anyhow::Result<String> {
        if command.trim().is_empty() {
            anyhow::bail!("command is required");
        }
        // Unpredictable action id: a sequential counter would let any
        // session's agent enumerate and read other sessions' background outputs
        // through status (which is RiskLevel::Safe).
        let id = haven_common::types::new_id("act");
        let started_at = chrono::Utc::now().to_rfc3339();
        let (kill_tx, kill_rx) = oneshot::channel();
        let tail = Arc::new(Mutex::new(String::new()));
        let tail_max_chars = *self.job_tail_max_chars.read().await;
        let emit_interval = *self.job_output_emit_interval.read().await;
        let terminal_ttl = *self.terminal_job_ttl.read().await;
        let max_actions = *self.max_actions.read().await;
        let _spawn_gate = self.spawn_gate.lock().await;
        {
            let mut actions = self.actions.write().await;
            // Reap terminal entries first: their results were already
            // delivered via the completion channel, so they must not occupy
            // the cap forever (64 lifetime actions would otherwise brick the
            // feature for long-lived sessions). Terminal entries older than
            // the configured terminal-action TTL are dropped the same way (the
            // UI panel and the persisted log files remain the record after
            // that).
            actions.retain(|_, e| !terminal_entry_stale(e, terminal_ttl));
            let running = actions
                .values()
                .filter(|e| matches!(e.state, BackgroundActionState::Running { .. }))
                .count();
            if running >= max_actions {
                anyhow::bail!(
                    "too many running background actions (limit {})",
                    max_actions
                );
            }
        }

        // Persist before publishing the action to the in-memory board or
        // starting a process. A failed database write therefore cannot leave a
        // process that restore_after_restart does not know how to clean up.
        if let Some(db) = self.db.read().await.clone() {
            let action_id = id.clone();
            let command_for_db = command.to_string();
            let started_at_for_db = started_at.clone();
            let session_id_for_db = session_id.map(str::to_owned);
            if let Err(error) = db
                .run_blocking(move |db| {
                    db.save_action(
                        &action_id,
                        session_id_for_db.as_deref(),
                        &command_for_db,
                        &started_at_for_db,
                    )
                })
                .await
            {
                tracing::warn!(action_id = %id, "failed to persist action spawn: {error}");
                return Err(error);
            }
        }

        self.actions.write().await.insert(
            id.clone(),
            BackgroundAction {
                session_id: session_id.map(str::to_owned),
                state: BackgroundActionState::Running {
                    started_at: started_at.clone(),
                },
                kill: Some(kill_tx),
                tail: Some(tail.clone()),
                command: command.to_string(),
                shell: shell.to_string(),
            },
        );

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
                // Spawn failed: remove the entry so the action is not left
                // dangling as "running".
                self.actions.write().await.remove(&id);
                if let Some(db) = self.db.read().await.clone() {
                    let action_id = id.clone();
                    if let Err(error) = db
                        .run_blocking(move |db| db.delete_action(&action_id))
                        .await
                    {
                        tracing::warn!(action_id = %id, "failed to remove action row after spawn failure: {error}");
                    }
                }
                return Err(e.into());
            }
        };

        let me = self.clone();
        let action_id = id.clone();
        let shell_owned = shell.to_string();
        let command_owned = command.to_string();
        self.emit(
            "action:created",
            json!({
                "action_id": action_id,
                "kind": "background",
                "status": "running",
                "started_at": started_at,
            }),
        );
        // The direct child pid is captured before `run` moves `child`; on
        // Windows, cancelling must kill the whole process tree, not just the
        // cmd.exe/powershell.exe wrapper.
        let child_pid = child.id();
        // The action runner outlives its spawner: give it a action-level span so
        // every log line emitted while the action runs/cancels (output-log
        // writes, completion) carries the action id — parallel background actions
        // stay distinguishable in logs.
        let action_span = tracing::info_span!("bg_action", action_id = %action_id);
        let runner_tail = tail.clone();
        let emit_action_id = action_id.clone();
        tokio::spawn(async move {
            // The action outlives this session: when `run` is dropped (kill signal
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
                    me.mark_cancelled(&action_id, &started_at).await;
                }
                (combined, success, exit_code, truncated) = &mut run => {
                    me.mark_finished(&action_id, &started_at, &shell_owned, &command_owned, combined, success, exit_code, truncated).await;
                }
            }
        }.instrument(action_span));

        // Live-output preview: emit `action:output` when the bounded tail
        // changes (by value — length alone freezes once the window is full).
        let emit_me = self.clone();
        let emit_tail = tail;
        tokio::spawn(async move {
            let mut last_output = String::new();
            loop {
                tokio::time::sleep(emit_interval).await;
                if emit_me.status(&emit_action_id).await["status"].as_str() != Some("running") {
                    return;
                }
                if take_tail_if_changed(&emit_tail, &mut last_output) {
                    emit_me.emit(
                        "action:output",
                        json!({
                            "action_id": emit_action_id,
                            "status": "running",
                            "output": last_output.as_str(),
                        }),
                    );
                }
            }
        });

        Ok(id)
    }

    /// Report the current status of a action as JSON.
    pub async fn status(&self, action_id: &str) -> Value {
        let actions = self.actions.read().await;
        let Some(entry) = actions.get(action_id) else {
            return json!({"action_id": action_id, "status": "not_found"});
        };
        Self::render_action_status(action_id, entry)
    }

    /// Status lookup scoped to the owning session. Agent-facing callers must
    /// never be able to enumerate another session's action by guessing its id.
    pub async fn status_for_session(&self, action_id: &str, session_id: &str) -> Value {
        let actions = self.actions.read().await;
        let Some(entry) = actions.get(action_id) else {
            return json!({"action_id": action_id, "status": "not_found"});
        };
        if entry.session_id.as_deref() != Some(session_id) {
            return json!({"action_id": action_id, "status": "not_found"});
        }
        Self::render_action_status(action_id, entry)
    }

    fn render_action_status(action_id: &str, entry: &BackgroundAction) -> Value {
        match &entry.state {
            BackgroundActionState::Running { .. } => {
                let body = running_status_json(action_id, entry);
                let mut ordered = haven_common::tools::background_wait_object(
                    "The action is still running. END YOUR TURN if you have nothing else useful to do — do not poll. The result is auto-pushed and the session is auto-woken when it finishes.",
                );
                // Move fields (including the live output tail) — do not clone
                // the potentially large `output` string just to reorder keys.
                if let Value::Object(obj) = body {
                    for (k, val) in obj {
                        ordered.insert(k, val);
                    }
                }
                Value::Object(ordered)
            }
            _ => render_status_json(action_id, &entry.state),
        }
    }

    /// Associate a action with its owning session. Called by the session executor
    /// after a background tool call so `cancel_for_session` can clean it up.
    ///
    /// Also closes a race: a short-lived action may finish (and call
    /// `mark_finished`/`mark_cancelled`) before this binding is recorded, in
    /// which case the completion notification carried `session_id: None` and was
    /// dropped by the consumer. If the action is already terminal here, re-fire
    /// the notification with the now-known session_id so the owning session still
    /// receives the result.
    pub async fn attach_session(&self, action_id: &str, session_id: &str) {
        let (terminal_state, session_id) = {
            let mut actions = self.actions.write().await;
            let Some(entry) = actions.get_mut(action_id) else {
                return;
            };
            if let Some(existing) = entry.session_id.as_deref() {
                if existing == session_id {
                    return;
                }
                tracing::warn!(
                    action_id,
                    existing_session_id = existing,
                    requested_session_id = session_id,
                    "refusing to rebind background action to another session"
                );
                return;
            }
            entry.session_id = Some(session_id.to_string());
            (
                entry.state.is_terminal().then(|| entry.state.clone()),
                session_id.to_string(),
            )
        };
        self.emit(
            "action:updated",
            json!({
                "action_id": action_id,
                "session_id": session_id,
            }),
        );
        // Record the owning session in the persisted row too, so terminal
        // history keeps its owner (spawn rows start with session_id NULL).
        if let Some(db) = self.db.read().await.clone() {
            let action_id = action_id.to_string();
            let action_id_for_db = action_id.clone();
            let session_id_for_db = session_id.clone();
            if let Err(e) = db
                .run_blocking(move |db| {
                    db.update_action_session(&action_id_for_db, &session_id_for_db)
                })
                .await
            {
                tracing::warn!(action_id = %action_id, "failed to persist action session binding: {e}");
            }
        }
        if let Some(state) = terminal_state {
            self.notify_completion(action_id, state, Some(session_id))
                .await;
        }
    }

    /// Cancel a single running action (kept for inspection afterwards).
    /// Returns false when the action does not exist or is not running.
    pub async fn cancel(&self, action_id: &str) -> bool {
        let mut actions = self.actions.write().await;
        let Some(entry) = actions.get_mut(action_id) else {
            return false;
        };
        if !matches!(entry.state, BackgroundActionState::Running { .. }) {
            return false;
        }
        if let Some(tx) = entry.kill.take() {
            let _ = tx.send(());
        }
        true
    }

    /// Cancel a single action only when it belongs to `session_id`.
    pub async fn cancel_for_session(&self, action_id: &str, session_id: &str) -> bool {
        let mut actions = self.actions.write().await;
        let Some(entry) = actions.get_mut(action_id) else {
            return false;
        };
        if entry.session_id.as_deref() != Some(session_id)
            || !matches!(entry.state, BackgroundActionState::Running { .. })
        {
            return false;
        }
        if let Some(tx) = entry.kill.take() {
            let _ = tx.send(());
        }
        true
    }

    /// Cancel and drop every action owned by `session_id`. Called when a session
    /// ends, is removed, or is rolled back.
    ///
    /// Running actions are killed, marked cancelled, persisted, and surfaced to
    /// the UI via `action:finished` before leaving the board — otherwise the
    /// titlebar panel keeps a ghost "running" row that cannot be stopped.
    pub async fn cancel_owned_by_session(&self, session_id: &str) {
        let ids: Vec<String> = {
            let actions = self.actions.read().await;
            actions
                .iter()
                .filter(|(_, e)| e.session_id.as_deref() == Some(session_id))
                .map(|(id, _)| id.clone())
                .collect()
        };
        for id in ids {
            let Some(mut entry) = self.actions.write().await.remove(&id) else {
                continue;
            };
            if let Some(tx) = entry.kill.take() {
                let _ = tx.send(());
            }
            entry.tail = None;
            if let BackgroundActionState::Running { started_at } = &entry.state {
                entry.state = BackgroundActionState::Cancelled {
                    started_at: started_at.clone(),
                    finished_at: chrono::Utc::now().to_rfc3339(),
                };
                self.notify_completion(&id, entry.state.clone(), entry.session_id.clone())
                    .await;
            } else if entry.state.is_terminal() {
                // UI-only: the board is dropping a row whose agent completion
                // already fired (or never needed one). Re-sending completion_tx
                // would risk duplicate inject on an ending session.
                let mut status_json = render_status_json(&id, &entry.state);
                if let Some(tid) = &entry.session_id {
                    status_json["session_id"] = json!(tid);
                }
                self.emit("action:finished", status_json);
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
        let (state, session_id) = {
            let mut actions = self.actions.write().await;
            let Some(entry) = actions.get_mut(id) else {
                return;
            };
            entry.kill = None;
            entry.tail = None;
            let finished_at = chrono::Utc::now().to_rfc3339();
            entry.state = if success {
                BackgroundActionState::Completed {
                    output: combined.clone(),
                    exit_code,
                    truncated,
                    // When the collected output was capped, the log file keeps
                    // the full transcript for inspection.
                    log_path: truncated.then(|| {
                        write_output_log("action-logs", id, &combined)
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
                BackgroundActionState::Failed {
                    error: combined.clone(),
                    error_reason: summarize_error(&diagnosed, 1200),
                    log_path: Some(
                        write_output_log("action-logs", id, &combined)
                            .to_string_lossy()
                            .into_owned(),
                    ),
                    exit_code,
                    started_at: started_at.to_string(),
                    finished_at,
                }
            };
            (entry.state.clone(), entry.session_id.clone())
        };
        self.notify_completion(id, state, session_id).await;
    }

    async fn mark_cancelled(&self, id: &str, started_at: &str) {
        let (state, session_id) = {
            let mut actions = self.actions.write().await;
            let Some(entry) = actions.get_mut(id) else {
                return;
            };
            entry.kill = None;
            entry.tail = None;
            entry.state = BackgroundActionState::Cancelled {
                started_at: started_at.to_string(),
                finished_at: chrono::Utc::now().to_rfc3339(),
            };
            (entry.state.clone(), entry.session_id.clone())
        };
        self.notify_completion(id, state, session_id).await;
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

/// Render the running-state row for a action: the command line it is executing
/// and the bounded live-output tail, so the agent sees what the action is doing
/// right now instead of only "running". `output` is omitted while empty (the
/// command has not produced anything yet).
fn running_status_json(action_id: &str, entry: &BackgroundAction) -> Value {
    let mut v = json!({
        "action_id": action_id,
        "status": "running",
        "command": entry.command,
        "shell": entry.shell,
    });
    if let BackgroundActionState::Running { started_at } = &entry.state {
        v["started_at"] = json!(started_at);
    }
    if let Some(tail) = &entry.tail {
        let out = lock_or_recover(tail, "action_output_tail");
        if !out.is_empty() {
            v["output"] = json!(out.as_str());
        }
    }
    v
}

/// Render the terminal status JSON for a action (mirrors `status()` output for
/// completed/failed/cancelled states), used in completion notifications.
fn render_status_json(action_id: &str, state: &BackgroundActionState) -> Value {
    match state {
        BackgroundActionState::Completed {
            output,
            exit_code,
            truncated,
            log_path,
            started_at,
            finished_at,
        } => {
            let mut v = json!({
                "action_id": action_id,
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
        BackgroundActionState::Failed {
            error,
            error_reason,
            log_path,
            exit_code,
            started_at,
            finished_at,
        } => {
            let mut v = json!({
                "action_id": action_id,
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
        BackgroundActionState::Cancelled {
            started_at,
            finished_at,
        } => json!({
            "action_id": action_id,
            "status": "cancelled",
            "started_at": started_at,
            "finished_at": finished_at,
        }),
        BackgroundActionState::Running { .. } => {
            json!({ "action_id": action_id, "status": "running" })
        }
    }
}

#[cfg(test)]
#[path = "background_actions_tests.rs"]
mod tests;
