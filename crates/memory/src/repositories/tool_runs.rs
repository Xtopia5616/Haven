use crate::db::Database;
use crate::repositories::messages::now_rfc3339_millis;
use anyhow::Context as _;
use chrono::{DateTime, SecondsFormat, Utc};
use haven_common::ToolRunCompletionPayload;
use haven_common::lifecycle::ToolRunStatus;

pub(super) fn normalize_tool_run_timestamp(value: &str, field: &str) -> anyhow::Result<String> {
    let timestamp = DateTime::parse_from_rfc3339(value)
        .with_context(|| format!("{field} must be an RFC3339 timestamp"))?;
    Ok(timestamp
        .with_timezone(&Utc)
        .to_rfc3339_opts(SecondsFormat::Millis, true))
}

/// A persisted detached background ToolRun row.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ToolRunRow {
    pub tool_run_id: String,
    pub kind: String,
    pub tool_name: String,
    pub session_id: Option<String>,
    /// Agent tool step that created a background ToolRun, when available.
    pub source_step_id: Option<String>,
    pub status: ToolRunStatus,
    pub output: Option<String>,
    pub error: Option<String>,
    pub error_reason: Option<String>,
    pub log_path: Option<String>,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
    pub created_at: String,
}

const TOOL_RUN_COLUMNS: &str = "id, kind, tool_name, session_id, source_step_id, status, output, error, error_reason, log_path, started_at, finished_at, created_at";

fn row_to_tool_run(row: &rusqlite::Row<'_>) -> rusqlite::Result<ToolRunRow> {
    Ok(ToolRunRow {
        tool_run_id: row.get(0)?,
        kind: row.get(1)?,
        tool_name: row.get(2)?,
        session_id: row.get(3)?,
        source_step_id: row.get(4)?,
        status: ToolRunStatus::from_status_str(&row.get::<_, String>(5)?),
        output: row.get(6)?,
        error: row.get(7)?,
        error_reason: row.get(8)?,
        log_path: row.get(9)?,
        started_at: row.get(10)?,
        finished_at: row.get(11)?,
        created_at: row.get(12)?,
    })
}

impl Database {
    /// Persist a newly spawned background ToolRun (status `running`).
    #[cfg(any(test, feature = "test-support"))]
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
    #[cfg(any(test, feature = "test-support"))]
    pub fn save_tool_run_with_source(
        &self,
        tool_run_id: &str,
        session_id: Option<&str>,
        _command: &str,
        started_at: &str,
        source_step_id: Option<&str>,
    ) -> anyhow::Result<()> {
        self.save_tool_run_with_metadata(
            tool_run_id,
            "shell",
            session_id,
            started_at,
            source_step_id,
        )
    }

    /// Persist a detached background invocation of one registered tool.
    pub fn save_tool_run_with_metadata(
        &self,
        tool_run_id: &str,
        tool_name: &str,
        session_id: Option<&str>,
        started_at: &str,
        source_step_id: Option<&str>,
    ) -> anyhow::Result<()> {
        anyhow::ensure!(
            !tool_name.trim().is_empty() && tool_name.len() <= 128,
            "invalid ToolRun tool_name"
        );
        let started_at = normalize_tool_run_timestamp(started_at, "started_at")?;
        let conn = self.conn();
        conn.execute(
            "INSERT INTO tool_runs (id, kind, tool_name, session_id, source_step_id, status, started_at, created_at)
             VALUES (?1, 'background', ?2, ?3, ?4, 'running', ?5, ?6)",
            rusqlite::params![tool_run_id, tool_name, session_id, source_step_id, started_at, now_rfc3339_millis()],
        )?;
        Ok(())
    }

    /// Bind a background ToolRun to its owning session after admission.
    pub fn update_tool_run_session(
        &self,
        tool_run_id: &str,
        session_id: &str,
    ) -> anyhow::Result<()> {
        let conn = self.conn();
        conn.execute_batch("BEGIN IMMEDIATE")?;
        let result = (|| {
            conn.execute(
                "UPDATE tool_runs SET session_id = ?2 WHERE id = ?1",
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

    /// Finalize a background ToolRun. Terminal rows remain available as history.
    #[allow(clippy::too_many_arguments)]
    pub fn finish_tool_run(
        &self,
        tool_run_id: &str,
        status: ToolRunStatus,
        output: Option<&str>,
        error: Option<&str>,
        error_reason: Option<&str>,
        log_path: Option<&str>,
        finished_at: &str,
    ) -> anyhow::Result<bool> {
        anyhow::ensure!(
            matches!(status, ToolRunStatus::Completed | ToolRunStatus::Failed),
            "background ToolRun terminal status must be completed or failed"
        );
        let finished_at = normalize_tool_run_timestamp(finished_at, "finished_at")?;
        let changed = self.conn().execute(
            "UPDATE tool_runs SET status = ?2, output = ?3, error = ?4, error_reason = ?5,
                    log_path = ?6, finished_at = ?7
             WHERE id = ?1 AND status = 'running'",
            rusqlite::params![
                tool_run_id,
                status.as_str(),
                output,
                error,
                error_reason,
                log_path,
                finished_at
            ],
        )?;
        Ok(changed > 0)
    }

    /// Persist cancellation of a running background ToolRun.
    pub fn cancel_background_tool_run(
        &self,
        tool_run_id: &str,
        finished_at: &str,
    ) -> anyhow::Result<bool> {
        let finished_at = normalize_tool_run_timestamp(finished_at, "finished_at")?;
        let changed = self.conn().execute(
            "UPDATE tool_runs SET status = 'cancelled', finished_at = ?2 WHERE id = ?1 AND status = 'running'",
            rusqlite::params![tool_run_id, finished_at],
        )?;
        Ok(changed > 0)
    }

    /// Finalize a background ToolRun and enqueue its result atomically.
    pub fn finish_tool_run_with_completion(
        &self,
        payload: &ToolRunCompletionPayload,
    ) -> anyhow::Result<bool> {
        anyhow::ensure!(
            !payload.tool_name.trim().is_empty() && payload.tool_name.len() <= 128,
            "background ToolRun completion requires a valid tool_name"
        );
        anyhow::ensure!(
            matches!(
                payload.status,
                ToolRunStatus::Completed | ToolRunStatus::Failed
            ),
            "background completion outbox only accepts completed or failed tool_runs"
        );
        let mut payload = payload.clone();
        payload.started_at = payload
            .started_at
            .as_deref()
            .map(|value| normalize_tool_run_timestamp(value, "started_at"))
            .transpose()?;
        payload.finished_at = payload
            .finished_at
            .as_deref()
            .map(|value| normalize_tool_run_timestamp(value, "finished_at"))
            .transpose()?;
        let finished_at = payload
            .finished_at
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("terminal ToolRun completion requires finished_at"))?
            .to_owned();
        let payload_json = serde_json::to_string(&payload)?;
        let conn = self.conn();
        conn.execute_batch("BEGIN IMMEDIATE")?;
        let result = (|| {
            let changed = conn.execute(
                "UPDATE tool_runs SET status = ?2, output = ?3, error = ?4, error_reason = ?5,
                        log_path = ?6, finished_at = ?7
                 WHERE id = ?1 AND status = 'running' AND tool_name = ?8",
                rusqlite::params![
                    payload.tool_run_id,
                    payload.status.as_str(),
                    payload.output,
                    payload.error,
                    payload.error_reason,
                    payload.log_path,
                    finished_at,
                    payload.tool_name
                ],
            )?;
            if changed == 0 {
                return Ok(false);
            }
            conn.execute(
                "INSERT OR IGNORE INTO tool_run_completion_outbox
                    (tool_run_id, tool_run_result_id, session_id, status, status_json)
                 SELECT id, id, session_id, ?2, ?3 FROM tool_runs WHERE id = ?1 AND status = ?2",
                rusqlite::params![payload.tool_run_id, payload.status.as_str(), payload_json],
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

    #[cfg(any(test, feature = "test-support"))]
    pub fn list_tool_runs(&self, kind: Option<&str>) -> anyhow::Result<Vec<ToolRunRow>> {
        self.list_tool_runs_for_session(kind, None)
    }

    pub fn list_tool_runs_for_session(
        &self,
        kind: Option<&str>,
        session_id: Option<&str>,
    ) -> anyhow::Result<Vec<ToolRunRow>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!("SELECT {TOOL_RUN_COLUMNS} FROM tool_runs WHERE (?1 IS NULL OR kind = ?1) AND (?2 IS NULL OR session_id = ?2) ORDER BY started_at DESC, created_at DESC"))?;
        let rows = stmt.query_map([kind, session_id], row_to_tool_run)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub(crate) fn list_terminal_tool_runs(
        &self,
        kind: Option<&str>,
        session_id: Option<&str>,
        limit: Option<usize>,
    ) -> anyhow::Result<Vec<ToolRunRow>> {
        let limit = limit
            .map(i64::try_from)
            .transpose()
            .map_err(|_| anyhow::anyhow!("ToolRun history limit exceeds SQLite range"))?;
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!("SELECT {TOOL_RUN_COLUMNS} FROM tool_runs WHERE status IN ('completed','failed','cancelled') AND (?1 IS NULL OR kind = ?1) AND (?2 IS NULL OR session_id = ?2) ORDER BY started_at DESC, created_at DESC LIMIT COALESCE(?3, -1)"))?;
        let rows = stmt.query_map((kind, session_id, limit), row_to_tool_run)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub(crate) fn list_tool_run_output_log_ids(&self) -> anyhow::Result<Vec<String>> {
        let conn = self.conn();
        let mut stmt =
            conn.prepare("SELECT id FROM tool_runs WHERE log_path IS NOT NULL ORDER BY id")?;
        stmt.query_map([], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    pub fn get_tool_run(&self, tool_run_id: &str) -> anyhow::Result<Option<ToolRunRow>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!(
            "SELECT {TOOL_RUN_COLUMNS} FROM tool_runs WHERE id = ?1"
        ))?;
        let mut rows = stmt.query_map([tool_run_id], row_to_tool_run)?;
        rows.next().transpose().map_err(Into::into)
    }

    pub fn delete_tool_run(&self, tool_run_id: &str) -> anyhow::Result<bool> {
        self.reconcile_tool_run_completion_outbox()?;
        let changed = self.conn().execute(
            "DELETE FROM tool_runs WHERE id = ?1 AND NOT EXISTS (SELECT 1 FROM tool_run_completion_outbox WHERE tool_run_id = ?1 AND delivered_at IS NULL)",
            [tool_run_id],
        )?;
        Ok(changed > 0)
    }

    pub fn clear_terminal_tool_runs(&self) -> anyhow::Result<Vec<String>> {
        self.reconcile_tool_run_completion_outbox()?;
        let conn = self.conn();
        conn.execute_batch("BEGIN IMMEDIATE")?;
        let result = (|| -> anyhow::Result<Vec<String>> {
            let ids = {
                let mut stmt = conn.prepare("SELECT id FROM tool_runs WHERE status IN ('completed','failed','cancelled') AND NOT EXISTS (SELECT 1 FROM tool_run_completion_outbox WHERE tool_run_id = tool_runs.id AND delivered_at IS NULL)")?;
                stmt.query_map([], |row| row.get::<_, String>(0))?
                    .collect::<Result<Vec<_>, _>>()?
            };
            let mut deleted = Vec::with_capacity(ids.len());
            for id in ids {
                if conn.execute("DELETE FROM tool_runs WHERE id = ?1 AND status IN ('completed','failed','cancelled') AND NOT EXISTS (SELECT 1 FROM tool_run_completion_outbox WHERE tool_run_id = ?1 AND delivered_at IS NULL)", [&id])? > 0 { deleted.push(id); }
            }
            Ok(deleted)
        })();
        match result {
            Ok(deleted) => {
                conn.execute_batch("COMMIT")?;
                Ok(deleted)
            }
            Err(error) => {
                let _ = conn.execute_batch("ROLLBACK");
                Err(error)
            }
        }
    }

    /// Mark ToolRuns left running by a previous process as failed.
    pub fn mark_interrupted_tool_runs(&self) -> anyhow::Result<usize> {
        let finished_at = now_rfc3339_millis();
        self.conn().execute("UPDATE tool_runs SET status = 'failed', error_reason = 'App restarted while the ToolRun was running', finished_at = ?1 WHERE status = 'running'", [&finished_at]).map_err(Into::into)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use haven_common::types::new_id;

    const STARTED: &str = "2026-09-24T10:00:00.000Z";
    const FINISHED: &str = "2026-09-24T11:00:00.000Z";

    #[test]
    fn background_tool_run_roundtrips_and_rejects_second_terminal_transition() {
        let db = Database::open_in_memory().unwrap();
        let id = new_id("toolrun");
        db.save_tool_run(&id, None, "echo ok", STARTED).unwrap();
        assert!(
            db.finish_tool_run(
                &id,
                ToolRunStatus::Completed,
                Some("ok"),
                None,
                None,
                None,
                FINISHED
            )
            .unwrap()
        );
        assert!(
            !db.finish_tool_run(
                &id,
                ToolRunStatus::Failed,
                None,
                Some("late"),
                None,
                None,
                FINISHED
            )
            .unwrap()
        );
        let row = db.get_tool_run(&id).unwrap().unwrap();
        assert_eq!(row.kind, "background");
        assert_eq!(row.status, ToolRunStatus::Completed);
        assert_eq!(row.output.as_deref(), Some("ok"));
        assert_eq!(row.tool_name, "shell");
    }

    #[test]
    fn completion_outbox_is_atomic_with_background_result_and_replayable() {
        let db = Database::open_in_memory().unwrap();
        let id = new_id("toolrun");
        db.save_tool_run(&id, None, "echo ok", STARTED).unwrap();
        let payload = ToolRunCompletionPayload {
            tool_run_id: id.clone(),
            tool_name: "shell".into(),
            status: ToolRunStatus::Completed,
            output: Some("ok".into()),
            error: None,
            error_reason: None,
            log_path: None,
            started_at: Some(STARTED.into()),
            finished_at: Some(FINISHED.into()),
            source_step_id: None,
            truncated: false,
        };
        assert!(db.finish_tool_run_with_completion(&payload).unwrap());
        let completion = db.claim_tool_run_completion().unwrap().unwrap();
        assert_eq!(completion.tool_run_id, id);
        assert_eq!(completion.payload.output.as_deref(), Some("ok"));
        assert!(
            db.acknowledge_tool_run_completion(&completion.tool_run_result_id)
                .unwrap()
        );
        assert!(db.claim_tool_run_completion().unwrap().is_none());
    }

    #[test]
    fn tool_run_timestamp_is_canonicalized_to_utc_milliseconds() {
        assert_eq!(
            normalize_tool_run_timestamp("2026-09-24T12:00:00+02:00", "started_at").unwrap(),
            "2026-09-24T10:00:00.000Z"
        );
    }
}
