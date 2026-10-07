use crate::db::Database;
use haven_common::ToolRunStatus;
use rusqlite::OptionalExtension;
use serde_json::json;

const SCHEDULED_EXECUTION_CLAIM_PREFIX: &str = "scheduled_execution_claim.";

fn scheduled_execution_claim_key(tool_run_id: &str) -> String {
    format!("{SCHEDULED_EXECUTION_CLAIM_PREFIX}{tool_run_id}")
}

/// A persisted scheduled ToolRun row. Scheduled ToolRuns survive app restarts:
/// `due_at` is stored in RFC3339, and the app re-arms pending ones on startup
/// (or fires overdue ones immediately). `mode` selects the fire behavior:
/// - `notify`: show a notification (title/body).
/// - `tool`: call the tool in `tool_name` with `tool_args` (JSON text).
/// - `continue`: resume the session in `session_id`, delivering `prompt` as the
///   continuation message; `session_id` is the session that scheduled the ToolRun.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ScheduledToolRunRow {
    pub tool_run_id: String,
    pub due_at: String,
    pub title: String,
    pub body: String,
    pub mode: String,
    pub session_id: Option<String>,
    pub tool_name: Option<String>,
    pub tool_args: Option<String>,
    pub prompt: Option<String>,
    pub watch_tool_run_id: Option<String>,
    pub status: ToolRunStatus,
    pub created_at: String,
}

impl Database {
    /// Persist a new (pending) scheduled ToolRun. `mode` selects the fire behavior
    /// (see [`ScheduledToolRunRow`]); `session_id`/`tool_name`/`tool_args` are the
    /// mode-specific payloads, `prompt` the optional continuation text.
    #[allow(clippy::too_many_arguments)]
    pub fn save_scheduled_tool_run(
        &self,
        tool_run_id: &str,
        due_at: &str,
        title: &str,
        body: &str,
        mode: &str,
        session_id: Option<&str>,
        tool_name: Option<&str>,
        tool_args: Option<&str>,
        prompt: Option<&str>,
        watch_tool_run_id: Option<&str>,
    ) -> anyhow::Result<()> {
        anyhow::ensure!(
            (!due_at.trim().is_empty()) != watch_tool_run_id.is_some(),
            "scheduled ToolRun requires exactly one of due_at or watch_tool_run_id"
        );
        let conn = self.conn();
        conn.execute(
            "INSERT INTO tool_runs (id, kind, due_at, title, body, mode, session_id, tool_name, tool_args, prompt, watch_tool_run_id, status, created_at)
             VALUES (?1, 'scheduled', ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, 'waiting', datetime('now'))",
            rusqlite::params![
                tool_run_id,
                due_at,
                title,
                body,
                mode,
                session_id,
                tool_name,
                tool_args,
                prompt,
                watch_tool_run_id
            ],
        )?;
        Ok(())
    }

    /// All scheduled tool_runs that are still waiting, ordered by due time ascending.
    /// Background-ToolRun rows (`kind = 'background'`) are excluded: they carry no
    /// due time and are listed via [`Database::list_tool_runs`].
    pub fn list_pending_scheduled_tool_runs(&self) -> anyhow::Result<Vec<ScheduledToolRunRow>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT id, due_at, title, body, mode, session_id, tool_name, tool_args, prompt, watch_tool_run_id, status, created_at
             FROM tool_runs WHERE kind = 'scheduled' AND status = 'waiting' ORDER BY due_at ASC",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(ScheduledToolRunRow {
                tool_run_id: row.get(0)?,
                due_at: row.get(1)?,
                title: row.get(2)?,
                body: row.get(3)?,
                mode: row.get(4)?,
                session_id: row.get(5)?,
                tool_name: row.get(6)?,
                tool_args: row.get(7)?,
                prompt: row.get(8)?,
                watch_tool_run_id: row.get(9)?,
                status: ToolRunStatus::from_status_str(&row.get::<_, String>(10)?),
                created_at: row.get(11)?,
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// All scheduled tool_runs that may still fire or await an execution claim.
    /// Unlike the pending restore query, this also includes `running` rows so
    /// owner cleanup can arbitrate against another service instance that has
    /// already persisted its timer transition.
    pub fn list_live_scheduled_tool_runs(&self) -> anyhow::Result<Vec<ScheduledToolRunRow>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT id, due_at, title, body, mode, session_id, tool_name, tool_args, prompt, watch_tool_run_id, status, created_at
             FROM tool_runs WHERE kind = 'scheduled' AND status IN ('waiting', 'running') ORDER BY due_at ASC",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(ScheduledToolRunRow {
                tool_run_id: row.get(0)?,
                due_at: row.get(1)?,
                title: row.get(2)?,
                body: row.get(3)?,
                mode: row.get(4)?,
                session_id: row.get(5)?,
                tool_name: row.get(6)?,
                tool_args: row.get(7)?,
                prompt: row.get(8)?,
                watch_tool_run_id: row.get(9)?,
                status: ToolRunStatus::from_status_str(&row.get::<_, String>(10)?),
                created_at: row.get(11)?,
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// Claim a scheduled ToolRun's trigger. Terminal rows remain durable history
    /// and are no longer re-armed on the next startup.
    pub fn start_scheduled_tool_run(
        &self,
        tool_run_id: &str,
        started_at: &str,
    ) -> anyhow::Result<bool> {
        let conn = self.conn();
        let changed = conn.execute(
            "UPDATE tool_runs SET status = 'running', started_at = ?2
             WHERE id = ?1 AND kind = 'scheduled' AND status = 'waiting'",
            rusqlite::params![tool_run_id, started_at],
        )?;
        Ok(changed > 0)
    }

    /// Put a scheduled ToolRun back into its durable waiting state when its
    /// trigger could not be delivered to a live consumer.
    pub fn requeue_scheduled_tool_run(&self, tool_run_id: &str) -> anyhow::Result<bool> {
        let conn = self.conn();
        let changed = conn.execute(
            "UPDATE tool_runs SET status = 'waiting', started_at = NULL, finished_at = NULL
             WHERE id = ?1 AND kind = 'scheduled' AND status = 'running'",
            rusqlite::params![tool_run_id],
        )?;
        Ok(changed > 0)
    }

    /// Finish a scheduled ToolRun after the actual trigger work has completed.
    /// Scheduled tool results share the durable ToolRun-result outbox with
    /// background tool_runs; `continue` mode keeps its existing input/transcript
    /// path and does not create a second completion result.
    pub fn finish_scheduled_tool_run(
        &self,
        tool_run_id: &str,
        status: ToolRunStatus,
        result_summary: Option<&str>,
        error_reason: Option<&str>,
        finished_at: &str,
    ) -> anyhow::Result<bool> {
        if !matches!(
            status,
            ToolRunStatus::Completed | ToolRunStatus::Failed | ToolRunStatus::Cancelled
        ) {
            anyhow::bail!("scheduled ToolRun terminal status must be terminal");
        }
        let conn = self.conn();
        conn.execute_batch("BEGIN IMMEDIATE")?;
        let result = (|| {
            let changed = conn.execute(
                "UPDATE tool_runs SET status = ?2, started_at = COALESCE(started_at, due_at),
                     result_summary = ?3, error_reason = ?4, finished_at = ?5
                 WHERE id = ?1 AND kind = 'scheduled' AND status = 'running'",
                rusqlite::params![
                    tool_run_id,
                    status.as_str(),
                    result_summary,
                    error_reason,
                    finished_at
                ],
            )?;
            if changed == 0 {
                return Ok(false);
            }
            conn.execute(
                "DELETE FROM kv_store WHERE key = ?1",
                [scheduled_execution_claim_key(tool_run_id)],
            )?;
            if matches!(status, ToolRunStatus::Completed | ToolRunStatus::Failed) {
                let mut status_json = json!({
                    "tool_run_id": tool_run_id,
                    "status": status.as_str(),
                    "finished_at": finished_at,
                });
                match status {
                    ToolRunStatus::Completed => {
                        status_json["output"] = json!(result_summary.unwrap_or_default());
                    }
                    ToolRunStatus::Failed => {
                        let error = error_reason.unwrap_or_default();
                        status_json["error"] = json!(error);
                        status_json["error_reason"] = json!(error);
                    }
                    ToolRunStatus::Waiting | ToolRunStatus::Running | ToolRunStatus::Cancelled => {
                        unreachable!("only completed/failed results are enqueued")
                    }
                }
                conn.execute(
                    "INSERT OR IGNORE INTO tool_run_completion_outbox
                         (tool_run_id, tool_run_result_id, session_id, status, status_json)
                     SELECT id, id, session_id, ?2, ?3
                     FROM tool_runs
                     WHERE id = ?1 AND kind = 'scheduled' AND mode = 'tool' AND status = ?2",
                    rusqlite::params![tool_run_id, status.as_str(), status_json.to_string()],
                )?;
            }
            Ok::<_, anyhow::Error>(true)
        })();
        match result {
            Ok(changed) => {
                conn.execute_batch("COMMIT")?;
                Ok(changed)
            }
            Err(error) => {
                let _ = conn.execute_batch("ROLLBACK");
                Err(error)
            }
        }
    }

    /// Read the durable status and model-facing result for one dependency
    /// producer. This projection is intentionally separate from UI/history.
    pub fn get_tool_run_dependency(
        &self,
        tool_run_id: &str,
    ) -> anyhow::Result<Option<ToolRunDependencyRow>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT status,
                    CASE
                        WHEN kind = 'scheduled' AND status = 'completed' THEN result_summary
                        WHEN status = 'completed' THEN output
                        WHEN status = 'failed' THEN COALESCE(NULLIF(error_reason, ''), NULLIF(error, ''))
                        ELSE NULL
                    END
             FROM tool_runs WHERE id = ?1",
        )?;
        let mut rows = stmt.query_map([tool_run_id], |row| {
            Ok(ToolRunDependencyRow {
                status: ToolRunStatus::from_status_str(&row.get::<_, String>(0)?),
                result: row.get(1)?,
            })
        })?;
        rows.next().transpose().map_err(Into::into)
    }

    /// Cancel a waiting or currently-running scheduled ToolRun while retaining
    /// its terminal history. An accepted confirmation's durable execution
    /// claim makes cancellation ineligible before this CAS runs.
    pub fn cancel_scheduled_tool_run(
        &self,
        tool_run_id: &str,
        finished_at: &str,
    ) -> anyhow::Result<bool> {
        let conn = self.conn();
        let claim_key = scheduled_execution_claim_key(tool_run_id);
        let changed = conn.execute(
            "UPDATE tool_runs SET status = 'cancelled', finished_at = ?2
             WHERE id = ?1 AND kind = 'scheduled' AND status IN ('waiting', 'running')
               AND NOT EXISTS (SELECT 1 FROM kv_store WHERE key = ?3)",
            rusqlite::params![tool_run_id, finished_at, claim_key],
        )?;
        Ok(changed > 0)
    }

    /// Durably arbitrate confirmation approval against scheduled ToolRun
    /// cancellation. The request ID is the idempotency token for retries after
    /// a later grant write fails. The claim and the ToolRun's running status are
    /// checked under one SQLite writer transaction.
    pub fn claim_scheduled_tool_run_execution(
        &self,
        tool_run_id: &str,
        request_id: &str,
    ) -> anyhow::Result<bool> {
        anyhow::ensure!(!request_id.trim().is_empty(), "request ID is required");
        let conn = self.conn();
        let key = scheduled_execution_claim_key(tool_run_id);
        conn.execute_batch("BEGIN IMMEDIATE")?;
        let result = (|| -> anyhow::Result<bool> {
            let status: Option<String> = conn
                .query_row(
                    "SELECT status FROM tool_runs WHERE id = ?1 AND kind = 'scheduled'",
                    [tool_run_id],
                    |row| row.get(0),
                )
                .optional()?;
            if status.as_deref() != Some("running") {
                return Ok(false);
            }
            let existing: Option<String> = conn
                .query_row("SELECT value FROM kv_store WHERE key = ?1", [&key], |row| {
                    row.get(0)
                })
                .optional()?;
            match existing {
                Some(existing) => Ok(existing == request_id),
                None => {
                    conn.execute(
                        "INSERT INTO kv_store (key, value) VALUES (?1, ?2)",
                        rusqlite::params![key, request_id],
                    )?;
                    Ok(true)
                }
            }
        })();
        match result {
            Ok(claimed) => {
                conn.execute_batch("COMMIT")?;
                Ok(claimed)
            }
            Err(error) => {
                let _ = conn.execute_batch("ROLLBACK");
                Err(error)
            }
        }
    }

    /// Release a claim after a retryable operation such as durable session
    /// grant persistence fails. A different request's claim is never removed.
    pub fn release_scheduled_tool_run_execution_claim(
        &self,
        tool_run_id: &str,
        request_id: &str,
    ) -> anyhow::Result<bool> {
        let conn = self.conn();
        let key = scheduled_execution_claim_key(tool_run_id);
        conn.execute_batch("BEGIN IMMEDIATE")?;
        let result = (|| -> anyhow::Result<bool> {
            let existing: Option<String> = conn
                .query_row("SELECT value FROM kv_store WHERE key = ?1", [&key], |row| {
                    row.get(0)
                })
                .optional()?;
            match existing {
                None => Ok(true),
                Some(existing) if existing == request_id => Ok(conn.execute(
                    "DELETE FROM kv_store WHERE key = ?1 AND value = ?2",
                    rusqlite::params![key, request_id],
                )? > 0),
                Some(_) => Ok(false),
            }
        })();
        match result {
            Ok(released) => {
                conn.execute_batch("COMMIT")?;
                Ok(released)
            }
            Err(error) => {
                let _ = conn.execute_batch("ROLLBACK");
                Err(error)
            }
        }
    }

    /// Quarantine a malformed waiting scheduled row as terminal history instead
    /// of leaving it invisible to the pending-ToolRun query forever.
    pub fn fail_waiting_scheduled_tool_run(
        &self,
        tool_run_id: &str,
        error_reason: &str,
        finished_at: &str,
    ) -> anyhow::Result<bool> {
        let conn = self.conn();
        let changed = conn.execute(
            "UPDATE tool_runs SET status = 'failed', error_reason = ?2, finished_at = ?3
             WHERE id = ?1 AND kind = 'scheduled' AND status = 'waiting'",
            rusqlite::params![tool_run_id, error_reason, finished_at],
        )?;
        Ok(changed > 0)
    }
}

/// A persisted ToolRun row (background or scheduled).
/// Scheduled-ToolRun rows carry `kind: "scheduled"` (due_at/mode/tool_name/
/// tool_args/prompt); background-ToolRun rows carry `kind: "background"` with the
/// ToolRun lifecycle fields (status/command/output/error/error_reason/log_path/
/// exit_code/started_at/finished_at). `status` is authoritative for both kinds.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ToolRunRow {
    pub tool_run_id: String,
    pub kind: String,
    pub due_at: Option<String>,
    pub title: String,
    pub body: Option<String>,
    pub mode: Option<String>,
    pub session_id: Option<String>,
    /// Agent tool step that created a background ToolRun, when available.
    pub source_step_id: Option<String>,
    pub tool_name: Option<String>,
    pub tool_args: Option<String>,
    pub prompt: Option<String>,
    pub status: ToolRunStatus,
    pub command: Option<String>,
    pub output: Option<String>,
    pub error: Option<String>,
    pub error_reason: Option<String>,
    pub log_path: Option<String>,
    pub exit_code: Option<i32>,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
    pub created_at: String,
}

/// Minimal persisted producer state used by scheduled dependency watchers.
/// The result is never included in ToolRun board or history projections.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ToolRunDependencyRow {
    pub status: ToolRunStatus,
    pub result: Option<String>,
}

const TOOL_RUN_COLUMNS: &str = "id, kind, due_at, title, body, mode, session_id, source_step_id, tool_name, tool_args, prompt, status, command, output, error, error_reason, log_path, exit_code, started_at, finished_at, created_at";

fn row_to_tool_run(row: &rusqlite::Row<'_>) -> rusqlite::Result<ToolRunRow> {
    Ok(ToolRunRow {
        tool_run_id: row.get(0)?,
        kind: row.get(1)?,
        due_at: row.get(2)?,
        title: row.get(3)?,
        body: row.get(4)?,
        mode: row.get(5)?,
        session_id: row.get(6)?,
        source_step_id: row.get(7)?,
        tool_name: row.get(8)?,
        tool_args: row.get(9)?,
        prompt: row.get(10)?,
        status: ToolRunStatus::from_status_str(&row.get::<_, String>(11)?),
        command: row.get(12)?,
        output: row.get(13)?,
        error: row.get(14)?,
        error_reason: row.get(15)?,
        log_path: row.get(16)?,
        exit_code: row.get(17)?,
        started_at: row.get(18)?,
        finished_at: row.get(19)?,
        created_at: row.get(20)?,
    })
}

impl Database {
    /// Persist a newly spawned background ToolRun (status `running`). The ToolRun is
    /// later finalized by [`Database::finish_tool_run`]; terminal rows stay in the
    /// table as history. Scheduled-only columns remain NULL for background ToolRuns
    /// because each kind owns its own payload fields.
    pub fn save_tool_run(
        &self,
        tool_run_id: &str,
        session_id: Option<&str>,
        command: &str,
        started_at: &str,
    ) -> anyhow::Result<()> {
        self.save_tool_run_with_source(tool_run_id, session_id, command, started_at, None)
    }

    /// Persist a background ToolRun with its originating Agent tool step.
    pub fn save_tool_run_with_source(
        &self,
        tool_run_id: &str,
        session_id: Option<&str>,
        command: &str,
        started_at: &str,
        source_step_id: Option<&str>,
    ) -> anyhow::Result<()> {
        let conn = self.conn();
        conn.execute(
            "INSERT INTO tool_runs (id, kind, session_id, source_step_id, command, status, started_at, created_at)
             VALUES (?1, 'background', ?2, ?3, ?4, 'running', ?5, datetime('now'))",
            rusqlite::params![tool_run_id, session_id, source_step_id, command, started_at],
        )?;
        Ok(())
    }

    /// Record the owning session of a background ToolRun (arrives after spawn via
    /// the tool manager's session binding).
    pub fn update_tool_run_session(
        &self,
        tool_run_id: &str,
        session_id: &str,
    ) -> anyhow::Result<()> {
        let conn = self.conn();
        conn.execute_batch("BEGIN IMMEDIATE")?;
        let result = (|| {
            conn.execute(
                "UPDATE tool_runs SET session_id = ?2 WHERE id = ?1 AND kind = 'background'",
                rusqlite::params![tool_run_id, session_id],
            )?;
            conn.execute(
                "UPDATE tool_run_completion_outbox
                 SET session_id = ?2, claimed_until = NULL, delivered_at = NULL
                 WHERE tool_run_id = ?1 AND session_id IS NULL",
                rusqlite::params![tool_run_id, session_id],
            )?;
            Ok::<_, anyhow::Error>(())
        })();
        match result {
            Ok(()) => {
                conn.execute_batch("COMMIT")?;
                Ok(())
            }
            Err(error) => {
                let _ = conn.execute_batch("ROLLBACK");
                Err(error)
            }
        }
    }

    /// Finalize a background ToolRun with its terminal status and payload. The
    /// row stays in the table as history; `output`/`error` are bounded
    /// summaries (the full transcript lives in the `log_path` file).
    #[allow(clippy::too_many_arguments)]
    pub fn finish_tool_run(
        &self,
        tool_run_id: &str,
        status: ToolRunStatus,
        output: Option<&str>,
        error: Option<&str>,
        error_reason: Option<&str>,
        log_path: Option<&str>,
        exit_code: Option<i32>,
        finished_at: &str,
    ) -> anyhow::Result<bool> {
        anyhow::ensure!(
            matches!(status, ToolRunStatus::Completed | ToolRunStatus::Failed),
            "background ToolRun terminal status must be completed or failed"
        );
        let conn = self.conn();
        let changed = conn.execute(
            "UPDATE tool_runs
             SET status = ?2, output = ?3, error = ?4, error_reason = ?5,
                 log_path = ?6, exit_code = ?7, finished_at = ?8
             WHERE id = ?1 AND kind = 'background' AND status = 'running'",
            rusqlite::params![
                tool_run_id,
                status.as_str(),
                output,
                error,
                error_reason,
                log_path,
                exit_code,
                finished_at
            ],
        )?;
        Ok(changed > 0)
    }

    /// Persist cancellation of a running background ToolRun. Cancellation has
    /// no transcript completion outbox entry, but still competes with process
    /// completion for the same single terminal transition.
    pub fn cancel_background_tool_run(
        &self,
        tool_run_id: &str,
        finished_at: &str,
    ) -> anyhow::Result<bool> {
        let conn = self.conn();
        let changed = conn.execute(
            "UPDATE tool_runs
             SET status = 'cancelled', finished_at = ?2
             WHERE id = ?1 AND kind = 'background' AND status = 'running'",
            rusqlite::params![tool_run_id, finished_at],
        )?;
        Ok(changed > 0)
    }

    /// Finalize a background ToolRun and enqueue its agent completion in one
    /// SQLite transaction. The outbox row is intentionally not acknowledged
    /// here; the agent acknowledges it only after transcript projection.
    #[allow(clippy::too_many_arguments)]
    pub fn finish_tool_run_with_completion(
        &self,
        tool_run_id: &str,
        status: ToolRunStatus,
        output: Option<&str>,
        error: Option<&str>,
        error_reason: Option<&str>,
        log_path: Option<&str>,
        exit_code: Option<i32>,
        finished_at: &str,
        status_json: &str,
    ) -> anyhow::Result<bool> {
        anyhow::ensure!(
            matches!(status, ToolRunStatus::Completed | ToolRunStatus::Failed),
            "background completion outbox only accepts completed or failed tool_runs"
        );
        let conn = self.conn();
        conn.execute_batch("BEGIN IMMEDIATE")?;
        let result = (|| {
            let changed = conn.execute(
                "UPDATE tool_runs
                 SET status = ?2, output = ?3, error = ?4, error_reason = ?5,
                     log_path = ?6, exit_code = ?7, finished_at = ?8
                 WHERE id = ?1 AND kind = 'background' AND status = 'running'",
                rusqlite::params![
                    tool_run_id,
                    status.as_str(),
                    output,
                    error,
                    error_reason,
                    log_path,
                    exit_code,
                    finished_at
                ],
            )?;
            if changed == 0 {
                return Ok(false);
            }
            conn.execute(
                "INSERT OR IGNORE INTO tool_run_completion_outbox
                     (tool_run_id, tool_run_result_id, session_id, status, status_json)
                 SELECT id, id, session_id, ?2, ?3
                 FROM tool_runs
                 WHERE id = ?1 AND kind = 'background' AND status = ?2",
                rusqlite::params![tool_run_id, status.as_str(), status_json],
            )?;
            Ok::<_, anyhow::Error>(true)
        })();
        match result {
            Ok(changed) => {
                conn.execute_batch("COMMIT")?;
                Ok(changed)
            }
            Err(error) => {
                let _ = conn.execute_batch("ROLLBACK");
                Err(error)
            }
        }
    }

    /// All persisted tool_runs, optionally filtered by kind (`"background"` /
    /// `"scheduled"`), newest first. Waiting rows are returned for board
    /// hydration; terminal rows remain available as history.
    pub fn list_tool_runs(&self, kind: Option<&str>) -> anyhow::Result<Vec<ToolRunRow>> {
        self.list_tool_runs_for_session(kind, None)
    }

    /// All persisted tool_runs for one owning session, optionally filtered by
    /// kind, newest first. Used by the session timeline to hydrate its
    /// bounded session-scoped ToolRun history after a switch or restart.
    pub fn list_tool_runs_for_session(
        &self,
        kind: Option<&str>,
        session_id: Option<&str>,
    ) -> anyhow::Result<Vec<ToolRunRow>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!(
            "SELECT {TOOL_RUN_COLUMNS} FROM tool_runs
             WHERE (?1 IS NULL OR kind = ?1)
               AND (?2 IS NULL OR session_id = ?2)
             ORDER BY started_at DESC, created_at DESC"
        ))?;
        let rows = stmt.query_map([kind, session_id], row_to_tool_run)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// One persisted ToolRun by `tool_run_id` (either kind).
    pub fn get_tool_run(&self, tool_run_id: &str) -> anyhow::Result<Option<ToolRunRow>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!(
            "SELECT {TOOL_RUN_COLUMNS} FROM tool_runs WHERE id = ?1"
        ))?;
        let mut rows = stmt.query_map([tool_run_id], row_to_tool_run)?;
        rows.next().transpose().map_err(Into::into)
    }

    /// Remove a persisted ToolRun (background or scheduled) by `tool_run_id`.
    pub fn delete_tool_run(&self, tool_run_id: &str) -> anyhow::Result<bool> {
        // Rebuild a missing background completion row before deciding whether
        // history is deletable. The delete predicate and acknowledgement both
        // run as SQLite writer statements, so they cannot race into deleting
        // an unacknowledged completion.
        self.reconcile_tool_run_completion_outbox()?;
        let conn = self.conn();
        let changed = conn.execute(
            "DELETE FROM tool_runs
             WHERE id = ?1
               AND NOT EXISTS (
                   SELECT 1
                   FROM tool_run_completion_outbox
                   WHERE tool_run_id = ?1 AND delivered_at IS NULL
               )",
            rusqlite::params![tool_run_id],
        )?;
        Ok(changed > 0)
    }

    /// Delete all terminal ToolRun history in one writer transaction. A
    /// background completion stays until its outbox entry has been delivered
    /// to the owning session; waiting and running work is never cleared.
    pub fn clear_terminal_tool_runs(&self) -> anyhow::Result<Vec<String>> {
        self.reconcile_tool_run_completion_outbox()?;
        let conn = self.conn();
        conn.execute_batch("BEGIN IMMEDIATE")?;
        let result = (|| -> anyhow::Result<Vec<String>> {
            let tool_run_ids = {
                let mut stmt = conn.prepare(
                    "SELECT id FROM tool_runs
                     WHERE status IN ('completed', 'failed', 'cancelled')
                       AND NOT EXISTS (
                           SELECT 1 FROM tool_run_completion_outbox
                           WHERE tool_run_id = tool_runs.id AND delivered_at IS NULL
                       )",
                )?;
                let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
                rows.collect::<Result<Vec<_>, _>>()?
            };
            let mut deleted_tool_run_ids = Vec::with_capacity(tool_run_ids.len());
            for tool_run_id in tool_run_ids {
                let changed = conn.execute(
                    "DELETE FROM tool_runs
                     WHERE id = ?1 AND status IN ('completed', 'failed', 'cancelled')
                       AND NOT EXISTS (
                           SELECT 1 FROM tool_run_completion_outbox
                           WHERE tool_run_id = ?1 AND delivered_at IS NULL
                       )",
                    rusqlite::params![tool_run_id],
                )?;
                if changed > 0 {
                    deleted_tool_run_ids.push(tool_run_id);
                }
            }
            Ok(deleted_tool_run_ids)
        })();
        match result {
            Ok(deleted_tool_run_ids) => {
                conn.execute_batch("COMMIT")?;
                Ok(deleted_tool_run_ids)
            }
            Err(error) => {
                let _ = conn.execute_batch("ROLLBACK");
                Err(error)
            }
        }
    }

    /// Mark background-ToolRun rows left `running` by a previous process as
    /// failed: child processes die with the app, so a `running` row after a
    /// restart is stale and must not surface as live work. Idempotent.
    pub fn mark_interrupted_tool_runs(&self) -> anyhow::Result<usize> {
        let conn = self.conn();
        conn.execute_batch("BEGIN IMMEDIATE")?;
        let result = (|| -> anyhow::Result<usize> {
            let n = conn.execute(
                "UPDATE tool_runs
                 SET status = 'failed', error_reason = 'App restarted while the ToolRun was running',
                     finished_at = datetime('now')
                 WHERE kind IN ('background', 'scheduled') AND status = 'running'",
                [],
            )?;
            // Scheduled confirmations and execution claims are process-local
            // work. Running rows are failed above rather than replayed, so no
            // approval claim may survive this recovery boundary.
            conn.execute(
                "DELETE FROM kv_store
                 WHERE substr(key, 1, length(?1)) = ?1",
                [SCHEDULED_EXECUTION_CLAIM_PREFIX],
            )?;
            Ok(n)
        })();
        match result {
            Ok(n) => {
                conn.execute_batch("COMMIT")?;
                Ok(n)
            }
            Err(error) => {
                let _ = conn.execute_batch("ROLLBACK");
                Err(error)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::db::Database;
    use haven_common::ToolRunStatus;
    use haven_common::types::new_id;

    fn test_db() -> Database {
        Database::open_in_memory().expect("create in-memory db")
    }

    #[test]
    fn save_and_list_pending() {
        let db = test_db();
        db.save_scheduled_tool_run(
            "toolrun-1",
            "2026-08-04T02:00:00+08:00",
            "Haven",
            "drink water",
            "tool",
            None,
            Some("notify"),
            Some(r#"{"title":"Haven","body":"drink water"}"#),
            None,
            None,
        )
        .unwrap();
        db.save_scheduled_tool_run(
            "toolrun-2",
            "2026-08-04T01:00:00+08:00",
            "Haven",
            "stand up",
            "continue",
            Some("ses-7"),
            None,
            None,
            Some("check the weather"),
            None,
        )
        .unwrap();
        db.save_scheduled_tool_run(
            "toolrun-3",
            "2026-08-04T03:00:00+08:00",
            "Haven",
            "backup",
            "tool",
            Some("ses-7"),
            Some("files"),
            Some(r#"{"operation":"read","path":"C:\\x"}"#),
            None,
            None,
        )
        .unwrap();
        let pending = db.list_pending_scheduled_tool_runs().unwrap();
        assert_eq!(pending.len(), 3);
        // Ordered by due_at ascending.
        assert_eq!(pending[0].tool_run_id, "toolrun-2");
        assert_eq!(pending[0].body, "stand up");
        assert_eq!(pending[0].mode, "continue");
        assert_eq!(pending[0].session_id.as_deref(), Some("ses-7"));
        assert_eq!(pending[0].prompt.as_deref(), Some("check the weather"));
        assert_eq!(pending[1].tool_run_id, "toolrun-1");
        assert_eq!(pending[1].mode, "tool");
        assert_eq!(pending[1].tool_name.as_deref(), Some("notify"));
        assert_eq!(pending[1].prompt, None);
        assert_eq!(pending[2].tool_run_id, "toolrun-3");
        assert_eq!(pending[2].mode, "tool");
        assert_eq!(pending[2].tool_name.as_deref(), Some("files"));
        assert!(pending[2].tool_args.as_deref().unwrap().contains("read"));
        assert_eq!(pending[0].status, ToolRunStatus::Waiting);
    }

    #[test]
    fn background_tool_run_source_step_is_persisted_and_listed() {
        let db = test_db();
        db.save_tool_run_with_source(
            "toolrun-source",
            Some("ses-source"),
            "echo source",
            "started",
            Some("step-source"),
        )
        .unwrap();

        let row = db.get_tool_run("toolrun-source").unwrap().unwrap();
        assert_eq!(row.source_step_id.as_deref(), Some("step-source"));
        let listed = db.list_tool_runs(Some("background")).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].source_step_id.as_deref(), Some("step-source"));
    }

    #[test]
    fn dependency_trigger_and_result_survive_without_entering_tool_run_projection() {
        let db = test_db();
        let producer_id = new_id("toolrun");
        let continuation_id = new_id("toolrun");
        db.save_tool_run(&producer_id, None, "echo result", "started")
            .unwrap();
        db.finish_tool_run(
            &producer_id,
            ToolRunStatus::Completed,
            Some("producer output"),
            None,
            None,
            None,
            Some(0),
            "finished",
        )
        .unwrap();
        db.save_scheduled_tool_run(
            &continuation_id,
            "",
            "After producer",
            "continue with result",
            "continue",
            Some("ses-owner"),
            None,
            None,
            None,
            Some(&producer_id),
        )
        .unwrap();

        let pending = db.list_pending_scheduled_tool_runs().unwrap();
        let continuation = pending.first().unwrap();
        assert_eq!(continuation.tool_run_id, continuation_id);
        assert!(continuation.due_at.is_empty());
        assert_eq!(
            continuation.watch_tool_run_id.as_deref(),
            Some(producer_id.as_str())
        );
        assert_eq!(
            db.get_tool_run_dependency(&producer_id)
                .unwrap()
                .unwrap()
                .result
                .as_deref(),
            Some("producer output")
        );
        let projected = db.get_tool_run(&continuation_id).unwrap().unwrap();
        assert!(projected.output.is_none());

        db.start_scheduled_tool_run(&continuation_id, "fired")
            .unwrap();
        db.finish_scheduled_tool_run(
            &continuation_id,
            ToolRunStatus::Completed,
            Some("scheduled tool result"),
            None,
            "finished",
        )
        .unwrap();
        let dependency = db
            .get_tool_run_dependency(&continuation_id)
            .unwrap()
            .unwrap();
        assert_eq!(dependency.status, ToolRunStatus::Completed);
        assert_eq!(dependency.result.as_deref(), Some("scheduled tool result"));
        assert!(
            db.get_tool_run(&continuation_id)
                .unwrap()
                .unwrap()
                .output
                .is_none()
        );
    }

    #[test]
    fn scheduled_trigger_requires_exactly_one_timer_or_dependency() {
        let db = test_db();
        let timer_and_dependency = db.save_scheduled_tool_run(
            &new_id("toolrun"),
            "2026-08-04T02:00:00Z",
            "Invalid",
            "both triggers",
            "continue",
            Some("ses-owner"),
            None,
            None,
            Some("prompt"),
            Some("toolrun-producer"),
        );
        assert!(timer_and_dependency.is_err());

        let no_trigger = db.save_scheduled_tool_run(
            &new_id("toolrun"),
            "",
            "Invalid",
            "no trigger",
            "continue",
            Some("ses-owner"),
            None,
            None,
            Some("prompt"),
            None,
        );
        assert!(no_trigger.is_err());
    }

    #[test]
    fn completed_scheduled_tool_runs_are_hidden_from_pending() {
        let db = test_db();
        db.save_scheduled_tool_run(
            "toolrun-1",
            "2026-08-04T02:00:00+08:00",
            "Haven",
            "x",
            "tool",
            None,
            Some("notify"),
            None,
            None,
            None,
        )
        .unwrap();
        db.start_scheduled_tool_run("toolrun-1", "2026-08-04T02:00:00Z")
            .unwrap();
        db.finish_scheduled_tool_run(
            "toolrun-1",
            ToolRunStatus::Completed,
            None,
            None,
            "2026-08-04T02:00:01Z",
        )
        .unwrap();
        assert!(db.list_pending_scheduled_tool_runs().unwrap().is_empty());
    }

    #[test]
    fn tool_run_history_can_be_filtered_by_session_for_timeline_hydration() {
        let db = test_db();
        let session_id = new_id("ses");
        let other_session_id = new_id("ses");
        let background_id = new_id("toolrun");
        let scheduled_id = new_id("toolrun");
        let other_id = new_id("toolrun");

        db.save_tool_run_with_source(
            &background_id,
            Some(&session_id),
            "echo done",
            "2026-08-04T01:00:00Z",
            None,
        )
        .unwrap();
        db.finish_tool_run(
            &background_id,
            ToolRunStatus::Completed,
            Some("done"),
            None,
            None,
            None,
            Some(0),
            "2026-08-04T01:00:01Z",
        )
        .unwrap();
        db.save_scheduled_tool_run(
            &scheduled_id,
            "2026-08-04T02:00:00Z",
            "Reminder",
            "Drink water",
            "notify",
            Some(&session_id),
            None,
            None,
            None,
            None,
        )
        .unwrap();
        db.save_scheduled_tool_run(
            &other_id,
            "2026-08-04T03:00:00Z",
            "Other reminder",
            "Stand up",
            "notify",
            Some(&other_session_id),
            None,
            None,
            None,
            None,
        )
        .unwrap();

        let rows = db
            .list_tool_runs_for_session(None, Some(&session_id))
            .unwrap();
        let ids = rows
            .into_iter()
            .map(|row| row.tool_run_id)
            .collect::<Vec<_>>();
        assert_eq!(ids.len(), 2);
        assert!(ids.contains(&background_id));
        assert!(ids.contains(&scheduled_id));
        assert!(!ids.contains(&other_id));
    }

    #[test]
    fn cancelled_scheduled_tool_runs_are_hidden_from_pending() {
        let db = test_db();
        db.save_scheduled_tool_run(
            "toolrun-1",
            "2026-08-04T02:00:00+08:00",
            "Haven",
            "x",
            "tool",
            None,
            Some("notify"),
            None,
            None,
            None,
        )
        .unwrap();
        db.cancel_scheduled_tool_run("toolrun-1", "2026-08-04T02:00:01Z")
            .unwrap();
        assert!(db.list_pending_scheduled_tool_runs().unwrap().is_empty());
    }

    #[test]
    fn scheduled_execution_claim_and_cancel_are_first_wins() {
        let db = test_db();
        for id in ["toolrun-approved", "toolrun-cancelled"] {
            db.save_scheduled_tool_run(
                id,
                "2026-08-04T02:00:00+08:00",
                "Haven",
                "confirm before execution",
                "tool",
                None,
                Some("notify"),
                None,
                None,
                None,
            )
            .unwrap();
            assert!(
                db.start_scheduled_tool_run(id, "2026-08-04T02:00:00Z")
                    .unwrap()
            );
        }

        assert!(
            db.claim_scheduled_tool_run_execution("toolrun-approved", "conf-approval")
                .unwrap()
        );
        assert!(
            db.claim_scheduled_tool_run_execution("toolrun-approved", "conf-approval")
                .unwrap(),
            "same-request retries must be idempotent"
        );
        assert!(
            !db.claim_scheduled_tool_run_execution("toolrun-approved", "conf-other")
                .unwrap()
        );
        assert!(
            !db.cancel_scheduled_tool_run("toolrun-approved", "cancel-lost")
                .unwrap()
        );
        assert_eq!(
            db.get_kv("scheduled_execution_claim.toolrun-approved")
                .unwrap()
                .as_deref(),
            Some("conf-approval")
        );
        assert!(
            db.finish_scheduled_tool_run(
                "toolrun-approved",
                ToolRunStatus::Completed,
                Some("done"),
                None,
                "finish-approved",
            )
            .unwrap()
        );
        assert_eq!(
            db.get_kv("scheduled_execution_claim.toolrun-approved")
                .unwrap(),
            None
        );

        assert!(
            db.cancel_scheduled_tool_run("toolrun-cancelled", "cancel-won")
                .unwrap()
        );
        assert!(
            !db.claim_scheduled_tool_run_execution("toolrun-cancelled", "conf-late")
                .unwrap()
        );
        assert_eq!(
            db.get_tool_run("toolrun-cancelled")
                .unwrap()
                .unwrap()
                .status,
            ToolRunStatus::Cancelled
        );
    }

    #[test]
    fn restarting_fails_running_tool_runs_and_clears_scheduled_claims() {
        let db = test_db();
        db.set_kv("scheduledXexecution_claim.keep", "unrelated")
            .unwrap();
        db.save_scheduled_tool_run(
            "toolrun-interrupted",
            "2026-08-04T02:00:00+08:00",
            "Haven",
            "claimed before restart",
            "tool",
            None,
            Some("notify"),
            None,
            None,
            None,
        )
        .unwrap();
        assert!(
            db.start_scheduled_tool_run("toolrun-interrupted", "started")
                .unwrap()
        );
        assert!(
            db.claim_scheduled_tool_run_execution("toolrun-interrupted", "conf-interrupted")
                .unwrap()
        );

        assert_eq!(db.mark_interrupted_tool_runs().unwrap(), 1);
        assert_eq!(
            db.get_tool_run("toolrun-interrupted")
                .unwrap()
                .unwrap()
                .status,
            ToolRunStatus::Failed
        );
        assert_eq!(
            db.get_kv("scheduled_execution_claim.toolrun-interrupted")
                .unwrap(),
            None
        );
        assert_eq!(
            db.get_kv("scheduledXexecution_claim.keep").unwrap(),
            Some("unrelated".into())
        );
    }

    #[test]
    fn scheduled_claim_cas_coordinates_separate_database_connections() {
        let database_path = std::env::temp_dir().join(format!(
            "haven-scheduled-claim-{}.db",
            haven_common::types::new_id("toolrun")
        ));
        let approver = Database::open(&database_path).unwrap();
        let canceller = Database::open(&database_path).unwrap();
        approver
            .save_scheduled_tool_run(
                "toolrun-shared-connection",
                "2026-08-04T02:00:00+08:00",
                "Haven",
                "arbitrate across connections",
                "tool",
                None,
                Some("notify"),
                None,
                None,
                None,
            )
            .unwrap();
        assert!(
            approver
                .start_scheduled_tool_run("toolrun-shared-connection", "started")
                .unwrap()
        );

        assert!(
            approver
                .claim_scheduled_tool_run_execution(
                    "toolrun-shared-connection",
                    "conf-shared-connection"
                )
                .unwrap()
        );
        assert!(
            !canceller
                .cancel_scheduled_tool_run("toolrun-shared-connection", "cancel-lost")
                .unwrap()
        );
        assert!(
            approver
                .release_scheduled_tool_run_execution_claim(
                    "toolrun-shared-connection",
                    "conf-shared-connection",
                )
                .unwrap()
        );
        assert!(
            canceller
                .cancel_scheduled_tool_run("toolrun-shared-connection", "cancel-won")
                .unwrap()
        );
        assert!(
            !approver
                .claim_scheduled_tool_run_execution("toolrun-shared-connection", "conf-late")
                .unwrap()
        );
        drop(canceller);
        drop(approver);
        let _ = std::fs::remove_file(database_path);
    }

    #[test]
    fn malformed_waiting_scheduled_tool_runs_can_be_quarantined() {
        let db = test_db();
        db.save_scheduled_tool_run(
            "toolrun-invalid",
            "not-a-timestamp",
            "Haven",
            "bad schedule",
            "tool",
            None,
            Some("notify"),
            None,
            None,
            None,
        )
        .unwrap();

        assert!(
            db.fail_waiting_scheduled_tool_run(
                "toolrun-invalid",
                "invalid due_at",
                "2026-08-04T02:00:01Z",
            )
            .unwrap()
        );
        assert!(db.list_pending_scheduled_tool_runs().unwrap().is_empty());
        let row = db.get_tool_run("toolrun-invalid").unwrap().unwrap();
        assert_eq!(row.status, ToolRunStatus::Failed);
        assert_eq!(row.error_reason.as_deref(), Some("invalid due_at"));
        assert_eq!(row.finished_at.as_deref(), Some("2026-08-04T02:00:01Z"));
    }

    #[test]
    fn tool_run_lifecycle_persists_and_updates() {
        let db = test_db();
        db.save_tool_run(
            "toolrun-1",
            Some("ses-9"),
            "echo hello",
            "2026-08-09T10:00:00Z",
        )
        .unwrap();
        db.save_tool_run("toolrun-2", None, "ping", "2026-08-09T10:01:00Z")
            .unwrap();

        // A running background ToolRun must not leak into the pending list.
        assert!(db.list_pending_scheduled_tool_runs().unwrap().is_empty());

        db.update_tool_run_session("toolrun-2", "ses-9").unwrap();
        db.finish_tool_run(
            "toolrun-1",
            ToolRunStatus::Completed,
            Some("hello"),
            None,
            None,
            None,
            Some(0),
            "2026-08-09T10:00:05Z",
        )
        .unwrap();
        db.finish_tool_run(
            "toolrun-2",
            ToolRunStatus::Failed,
            None,
            Some("connection refused"),
            Some("connection refused"),
            Some("C:\\tmp\\tool-run-logs\\toolrun-2.log"),
            Some(1),
            "2026-08-09T10:01:03Z",
        )
        .unwrap();

        let all = db.list_tool_runs(None).unwrap();
        assert_eq!(all.len(), 2);
        let backgrounds = db.list_tool_runs(Some("background")).unwrap();
        assert_eq!(backgrounds.len(), 2);
        assert!(db.list_tool_runs(Some("scheduled")).unwrap().is_empty());

        let finished = backgrounds
            .iter()
            .find(|row| row.tool_run_id == "toolrun-1")
            .unwrap();
        assert_eq!(finished.status, ToolRunStatus::Completed);
        assert_eq!(finished.output.as_deref(), Some("hello"));
        assert_eq!(finished.exit_code, Some(0));
        assert_eq!(finished.session_id.as_deref(), Some("ses-9"));
        assert!(finished.finished_at.is_some());

        let failed = backgrounds
            .iter()
            .find(|row| row.tool_run_id == "toolrun-2")
            .unwrap();
        assert_eq!(failed.status, ToolRunStatus::Failed);
        assert_eq!(failed.session_id.as_deref(), Some("ses-9"));
        assert_eq!(
            failed.log_path.as_deref(),
            Some("C:\\tmp\\tool-run-logs\\toolrun-2.log")
        );
        assert_eq!(failed.error_reason.as_deref(), Some("connection refused"));

        assert!(db.get_tool_run("toolrun-1").unwrap().is_some());
        assert!(db.get_tool_run("nope").unwrap().is_none());
    }

    #[test]
    fn background_tool_run_terminal_writes_only_apply_once() {
        let db = test_db();
        for (id, first_status, late_status) in [
            (
                "toolrun-completed",
                ToolRunStatus::Completed,
                ToolRunStatus::Failed,
            ),
            (
                "toolrun-failed",
                ToolRunStatus::Failed,
                ToolRunStatus::Completed,
            ),
            (
                "toolrun-cancelled",
                ToolRunStatus::Cancelled,
                ToolRunStatus::Failed,
            ),
        ] {
            db.save_tool_run(id, None, "echo lifecycle", "started")
                .unwrap();
            if first_status == ToolRunStatus::Cancelled {
                assert!(db.cancel_background_tool_run(id, "first finish").unwrap());
            } else {
                assert!(
                    db.finish_tool_run(
                        id,
                        first_status,
                        Some("first output"),
                        Some("first error"),
                        Some("first reason"),
                        None,
                        Some(1),
                        "first finish",
                    )
                    .unwrap()
                );
            }
            let first = db.get_tool_run(id).unwrap().unwrap();

            assert!(
                !db.finish_tool_run(
                    id,
                    late_status,
                    Some("late output"),
                    Some("late error"),
                    Some("late reason"),
                    Some("late.log"),
                    Some(2),
                    "late finish",
                )
                .unwrap()
            );
            assert!(
                !db.cancel_background_tool_run(id, "late cancellation")
                    .unwrap()
            );

            let after_late_write = db.get_tool_run(id).unwrap().unwrap();
            assert_eq!(after_late_write.status, first.status, "{id}");
            assert_eq!(after_late_write.output, first.output, "{id}");
            assert_eq!(after_late_write.error, first.error, "{id}");
            assert_eq!(after_late_write.error_reason, first.error_reason, "{id}");
            assert_eq!(after_late_write.log_path, first.log_path, "{id}");
            assert_eq!(after_late_write.exit_code, first.exit_code, "{id}");
            assert_eq!(after_late_write.finished_at, first.finished_at, "{id}");
        }
    }

    #[test]
    fn background_finish_rejects_cancelled_status() {
        let db = test_db();
        db.save_tool_run("toolrun-cancel-api", None, "echo cancel", "started")
            .unwrap();

        assert!(
            db.finish_tool_run(
                "toolrun-cancel-api",
                ToolRunStatus::Cancelled,
                None,
                None,
                None,
                None,
                None,
                "finished",
            )
            .is_err()
        );
        assert!(
            db.cancel_background_tool_run("toolrun-cancel-api", "finished")
                .unwrap()
        );
        assert_eq!(
            db.get_tool_run("toolrun-cancel-api")
                .unwrap()
                .unwrap()
                .status,
            ToolRunStatus::Cancelled
        );
    }

    #[test]
    fn late_completion_cannot_overwrite_restart_failure() {
        let db = test_db();
        db.save_tool_run(
            "toolrun-restarted",
            Some("ses-1"),
            "echo restart",
            "started",
        )
        .unwrap();
        assert_eq!(db.mark_interrupted_tool_runs().unwrap(), 1);
        let interrupted = db.get_tool_run("toolrun-restarted").unwrap().unwrap();

        assert!(
            !db.finish_tool_run_with_completion(
                "toolrun-restarted",
                ToolRunStatus::Completed,
                Some("late output"),
                None,
                None,
                None,
                Some(0),
                "late finish",
                r#"{"tool_run_id":"toolrun-restarted","status":"completed","output":"late output"}"#,
            )
            .unwrap()
        );

        let after_late_completion = db.get_tool_run("toolrun-restarted").unwrap().unwrap();
        assert_eq!(after_late_completion.status, ToolRunStatus::Failed);
        assert_eq!(after_late_completion.error_reason, interrupted.error_reason);
        assert_eq!(after_late_completion.finished_at, interrupted.finished_at);
        assert!(after_late_completion.output.is_none());
        let outbox_count: i64 = db
            .conn()
            .query_row(
                "SELECT COUNT(*) FROM tool_run_completion_outbox WHERE tool_run_id = ?1",
                ["toolrun-restarted"],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(outbox_count, 0);
    }

    #[test]
    fn scheduled_rows_keep_kind_and_terminal_history() {
        let db = test_db();
        db.save_scheduled_tool_run(
            "toolrun-1",
            "2026-08-04T02:00:00+08:00",
            "Haven",
            "drink water",
            "tool",
            None,
            Some("notify"),
            None,
            None,
            None,
        )
        .unwrap();
        db.start_scheduled_tool_run("toolrun-1", "2026-08-04T02:00:00Z")
            .unwrap();
        db.finish_scheduled_tool_run(
            "toolrun-1",
            ToolRunStatus::Completed,
            None,
            None,
            "2026-08-04T02:00:01Z",
        )
        .unwrap();
        db.save_tool_run("toolrun-2", None, "echo", "2026-08-09T10:00:00Z")
            .unwrap();

        // Pending list excludes both the completed scheduled ToolRun and the background ToolRun.
        assert!(db.list_pending_scheduled_tool_runs().unwrap().is_empty());
        // ToolRun listing still surfaces the completed scheduled ToolRun as history.
        let scheduled = db.list_tool_runs(Some("scheduled")).unwrap();
        assert_eq!(scheduled.len(), 1);
        assert_eq!(scheduled[0].tool_run_id, "toolrun-1");
        assert_eq!(scheduled[0].status, ToolRunStatus::Completed);
        assert_eq!(scheduled[0].kind, "scheduled");
    }

    #[test]
    fn interrupted_tool_runs_marked_failed() {
        let db = test_db();
        db.save_tool_run("toolrun-1", None, "echo", "2026-08-09T10:00:00Z")
            .unwrap();
        db.save_tool_run("toolrun-2", None, "ping", "2026-08-09T10:01:00Z")
            .unwrap();
        db.save_scheduled_tool_run(
            "toolrun-3",
            "2026-08-09T10:02:00Z",
            "Scheduled",
            "resume",
            "tool",
            None,
            Some("notify"),
            None,
            None,
            None,
        )
        .unwrap();
        db.start_scheduled_tool_run("toolrun-3", "2026-08-09T10:02:00Z")
            .unwrap();
        db.finish_tool_run(
            "toolrun-1",
            ToolRunStatus::Completed,
            Some("ok"),
            None,
            None,
            None,
            Some(0),
            "2026-08-09T10:00:05Z",
        )
        .unwrap();

        let n = db.mark_interrupted_tool_runs().unwrap();
        assert_eq!(n, 2);
        let backgrounds = db.list_tool_runs(Some("background")).unwrap();
        let running_left = backgrounds
            .iter()
            .filter(|a| a.status == ToolRunStatus::Running)
            .count();
        assert_eq!(running_left, 0);
        let j2 = backgrounds
            .iter()
            .find(|row| row.tool_run_id == "toolrun-2")
            .unwrap();
        assert_eq!(j2.status, ToolRunStatus::Failed);
        assert!(j2.error_reason.as_deref().unwrap().contains("restarted"));
        let scheduled = db.get_tool_run("toolrun-3").unwrap().unwrap();
        assert_eq!(scheduled.status, ToolRunStatus::Failed);
        assert!(
            scheduled
                .error_reason
                .as_deref()
                .unwrap()
                .contains("restarted")
        );
        // Second run is a no-op.
        assert_eq!(db.mark_interrupted_tool_runs().unwrap(), 0);
    }

    #[test]
    fn delete_removes_any_kind() {
        let db = test_db();
        db.save_tool_run("toolrun-1", None, "echo", "2026-08-09T10:00:00Z")
            .unwrap();
        assert!(db.delete_tool_run("toolrun-1").unwrap());
        assert!(!db.delete_tool_run("toolrun-1").unwrap());
        assert!(db.list_tool_runs(Some("background")).unwrap().is_empty());
    }

    #[test]
    fn clear_terminal_tool_runs_keeps_live_rows_and_undelivered_completions() {
        let db = test_db();
        let waiting_id = new_id("toolrun");
        let running_id = new_id("toolrun");
        let cancelled_id = new_id("toolrun");
        let completed_id = new_id("toolrun");
        let pending_completion_id = new_id("toolrun");
        let now = "2026-10-06T00:00:00Z";

        for id in [&waiting_id, &running_id, &cancelled_id] {
            db.save_scheduled_tool_run(
                id,
                now,
                "Haven",
                "body",
                "tool",
                None,
                Some("notify"),
                Some("{}"),
                None,
                None,
            )
            .unwrap();
        }
        assert!(db.start_scheduled_tool_run(&running_id, now).unwrap());
        assert!(db.start_scheduled_tool_run(&cancelled_id, now).unwrap());
        assert!(db.cancel_scheduled_tool_run(&cancelled_id, now).unwrap());

        db.save_tool_run(&completed_id, None, "echo done", now)
            .unwrap();
        assert!(
            db.finish_tool_run(
                &completed_id,
                ToolRunStatus::Completed,
                Some("done"),
                None,
                None,
                None,
                Some(0),
                now,
            )
            .unwrap()
        );
        db.reconcile_tool_run_completion_outbox().unwrap();
        assert!(db.acknowledge_tool_run_completion(&completed_id).unwrap());
        db.save_tool_run(&pending_completion_id, None, "echo pending", now)
            .unwrap();
        assert!(
            db.finish_tool_run_with_completion(
                &pending_completion_id,
                ToolRunStatus::Completed,
                Some("result"),
                None,
                None,
                None,
                Some(0),
                now,
                r#"{"tool_run_id":"toolrun-pending","status":"completed","output":"result"}"#,
            )
            .unwrap()
        );

        let mut deleted = db.clear_terminal_tool_runs().unwrap();
        deleted.sort();
        let mut expected = vec![cancelled_id.clone(), completed_id.clone()];
        expected.sort();
        assert_eq!(deleted, expected);

        assert!(db.get_tool_run(&waiting_id).unwrap().is_some());
        assert!(db.get_tool_run(&running_id).unwrap().is_some());
        assert!(db.get_tool_run(&pending_completion_id).unwrap().is_some());
        assert!(db.get_tool_run(&cancelled_id).unwrap().is_none());
        assert!(db.get_tool_run(&completed_id).unwrap().is_none());
    }
}
