use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::sync::{RwLock, mpsc, oneshot};

/// Maximum concurrent *running* background jobs per process. Prevents an
/// agent from leaking unbounded child processes. Finished jobs are reaped
/// on the next spawn, so this is a concurrency cap, not a lifetime cap.
const MAX_JOBS: usize = 64;

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
    std_cmd
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
        truncated: bool,
        started_at: String,
        finished_at: String,
    },
    Failed {
        error: String,
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
        }
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

    /// Spawn a shell command as a background job. Returns the job id; the
    /// command keeps running after this function returns.
    pub async fn spawn_shell(
        self: &Arc<Self>,
        command: &str,
        shell: &str,
        max_chars: usize,
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
            if running >= MAX_JOBS {
                anyhow::bail!("too many running background jobs (limit {})", MAX_JOBS);
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

        let std_cmd = build_shell_command(shell, command);

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
                let success = matches!(status, Ok(s) if s.success());
                let truncated = stdout_overflow || stderr_overflow;
                (combined, success, truncated)
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
                (combined, success, truncated) = &mut run => {
                    me.mark_finished(&job_id, &started_at, combined, success, truncated).await;
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
                "hint": "The job is still running. Poll again with the status tool.",
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
        combined: String,
        success: bool,
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
                output: combined,
                truncated,
                started_at: started_at.to_string(),
                finished_at,
            }
        } else {
            JobState::Failed {
                error: combined,
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
            truncated,
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
            if *truncated {
                v["truncated"] = json!(true);
            }
            v
        }
        JobState::Failed {
            error,
            started_at,
            finished_at,
        } => json!({
            "job_id": job_id,
            "status": "failed",
            "error": error,
            "started_at": started_at,
            "finished_at": finished_at,
        }),
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
        let id = jobs.spawn_shell("echo done", "cmd", 20_000).await.unwrap();
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
        let id = jobs.spawn_shell("echo fast", "cmd", 20_000).await.unwrap();
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
        // No jobs → no completion. Just confirm the receiver is taken.
        let _rx = jobs.take_completion_receiver().expect("receiver available");
        // status on not_found doesn't notify.
        assert_eq!(jobs.status("nope").await["status"], "not_found");
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn test_spawn_shell_completes_with_output() {
        let jobs = Arc::new(BackgroundJobs::new());
        let id = jobs
            .spawn_shell("echo bg-hello", "cmd", 20_000)
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
        let id = jobs.spawn_shell("exit 7", "cmd", 20_000).await.unwrap();
        let v = wait_terminal(&jobs, &id, 10).await;
        assert_eq!(v["status"], "failed", "got: {}", v);
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn test_spawn_shell_stderr_captured() {
        let jobs = Arc::new(BackgroundJobs::new());
        let id = jobs
            .spawn_shell("echo err-msg 1>&2", "cmd", 20_000)
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
            .spawn_shell("ping -n 30 127.0.0.1", "cmd", 20_000)
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
            .spawn_shell("ping -n 30 127.0.0.1", "cmd", 20_000)
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
        assert!(jobs.spawn_shell("  ", "cmd", 20_000).await.is_err());
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
}
