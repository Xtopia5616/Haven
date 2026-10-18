//! Durable delivery records for terminal ToolRun results.
//!
//! The ToolRun row remains the source of the result payload. This repository
//! only records the delivery lifecycle so a transient completion broadcast can
//! be rebuilt and acknowledged after the owning session has durably projected
//! the result. Only detached background ToolRuns use this outbox; Schedule
//! runs have an independent lifecycle.

use crate::db::Database;
use haven_common::lifecycle::ToolRunStatus;
use haven_common::{ToolRunCompletionPayload, ToolRunLease};

const CLAIM_LEASE_SECS: i64 = 30;

#[derive(Debug, Clone)]
pub struct ToolRunCompletionOutboxRow {
    pub tool_run_id: String,
    pub tool_run_result_id: String,
    pub kind: String,
    pub session_id: Option<String>,
    pub status: ToolRunStatus,
    pub payload: ToolRunCompletionPayload,
}

#[allow(clippy::too_many_arguments)]
fn completion_payload(
    tool_run_id: &str,
    tool_name: &str,
    status: ToolRunStatus,
    output: Option<&str>,
    error: Option<&str>,
    error_reason: Option<&str>,
    log_path: Option<&str>,
    started_at: Option<&str>,
    finished_at: Option<&str>,
    source_step_id: Option<&str>,
) -> ToolRunCompletionPayload {
    ToolRunCompletionPayload {
        tool_run_id: tool_run_id.to_owned(),
        tool_name: tool_name.to_owned(),
        status,
        output: output.map(str::to_owned),
        error: error.map(str::to_owned),
        error_reason: error_reason.map(str::to_owned),
        log_path: log_path.map(str::to_owned),
        started_at: started_at.map(str::to_owned),
        finished_at: finished_at.map(str::to_owned),
        source_step_id: source_step_id.map(str::to_owned),
        truncated: false,
    }
}

impl Database {
    /// Rebuild missing outbox rows from terminal ToolRun history. This is the
    /// crash-recovery half of the outbox: a process can die after the ToolRun
    /// row commits but before the completion record is inserted.
    pub(crate) fn reconcile_tool_run_completion_outbox(&self) -> anyhow::Result<()> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT id, kind, tool_name, session_id, source_step_id, status, output,
                    error, error_reason, log_path, started_at, finished_at
             FROM tool_runs
             WHERE status IN ('completed', 'failed')
             ORDER BY created_at ASC, id ASC",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, Option<String>>(6)?,
                row.get::<_, Option<String>>(7)?,
                row.get::<_, Option<String>>(8)?,
                row.get::<_, Option<String>>(9)?,
                row.get::<_, Option<String>>(10)?,
                row.get::<_, Option<String>>(11)?,
            ))
        })?;
        let mut terminal = Vec::new();
        for row in rows {
            terminal.push(row?);
        }
        drop(stmt);

        conn.execute_batch("BEGIN IMMEDIATE")?;
        let result = (|| {
            for (
                tool_run_id,
                kind,
                tool_name,
                session_id,
                source_step_id,
                status,
                output,
                error,
                error_reason,
                log_path,
                started_at,
                finished_at,
            ) in terminal
            {
                let status = ToolRunStatus::from_status_str(&status);
                let payload = serde_json::to_string(&completion_payload(
                    &tool_run_id,
                    &tool_name,
                    status,
                    output.as_deref(),
                    error.as_deref(),
                    error_reason.as_deref(),
                    log_path.as_deref(),
                    started_at.as_deref(),
                    finished_at.as_deref(),
                    source_step_id.as_deref(),
                ))?;
                debug_assert_eq!(kind, "background");
                conn.execute(
                    "INSERT OR IGNORE INTO tool_run_completion_outbox
                         (tool_run_id, tool_run_result_id, session_id, status, status_json)
                     VALUES (?1, ?1, ?2, ?3, ?4)",
                    rusqlite::params![tool_run_id, session_id, status.as_str(), payload],
                )?;
            }
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

    /// Claim the oldest undelivered completion. Claims are short leases so a
    /// crashed agent consumer does not strand a durable result forever.
    pub fn claim_tool_run_completion(&self) -> anyhow::Result<Option<ToolRunCompletionOutboxRow>> {
        self.reconcile_tool_run_completion_outbox()?;
        let conn = self.conn();
        conn.execute_batch("BEGIN IMMEDIATE")?;
        let result = (|| {
            let row = conn
                .query_row(
                    "SELECT outbox.tool_run_id, outbox.tool_run_result_id, tool_runs.kind,
                            tool_runs.tool_name,
                            outbox.session_id, outbox.status, outbox.status_json,
                            claimed_until, datetime('now')
                     FROM tool_run_completion_outbox AS outbox
                     JOIN tool_runs ON tool_runs.id = outbox.tool_run_id
                     WHERE outbox.delivered_at IS NULL
                       AND (outbox.claimed_until IS NULL OR outbox.claimed_until <= datetime('now'))
                     ORDER BY outbox.created_at ASC, outbox.tool_run_id ASC
                     LIMIT 1",
                    [],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, String>(2)?,
                            row.get::<_, String>(3)?,
                            row.get::<_, Option<String>>(4)?,
                            row.get::<_, String>(5)?,
                            row.get::<_, String>(6)?,
                            row.get::<_, Option<String>>(7)?,
                            row.get::<_, String>(8)?,
                        ))
                    },
                )
                .optional()?;
            let Some((
                tool_run_id,
                tool_run_result_id,
                kind,
                tool_name,
                session_id,
                status,
                payload_json,
                claimed_until,
                now,
            )) = row
            else {
                return Ok(None);
            };
            let current_lease = claimed_until
                .map(|expires_at| ToolRunLease::new(tool_run_result_id.clone(), expires_at));
            if !ToolRunLease::can_claim(current_lease.as_ref(), &now) {
                return Ok(None);
            }
            conn.execute(
                "UPDATE tool_run_completion_outbox
                 SET claimed_until = datetime('now', ?2)
                 WHERE tool_run_id = ?1 AND delivered_at IS NULL",
                rusqlite::params![tool_run_id, format!("+{CLAIM_LEASE_SECS} seconds")],
            )?;
            let payload: ToolRunCompletionPayload = serde_json::from_str(&payload_json)?;
            anyhow::ensure!(
                payload.tool_run_id == tool_run_id && payload.tool_name == tool_name,
                "ToolRun completion payload identity does not match its durable row"
            );
            Ok(Some(ToolRunCompletionOutboxRow {
                tool_run_id,
                tool_run_result_id,
                kind,
                session_id,
                status: ToolRunStatus::from_status_str(&status),
                payload,
            }))
        })();
        match result {
            Ok(value) => {
                conn.execute_batch("COMMIT")?;
                Ok(value)
            }
            Err(error) => {
                let _ = conn.execute_batch("ROLLBACK");
                Err(error)
            }
        }
    }

    /// Mark a result delivered only after its transcript projection commits.
    pub fn acknowledge_tool_run_completion(
        &self,
        tool_run_result_id: &str,
    ) -> anyhow::Result<bool> {
        let conn = self.conn();
        let changed = conn.execute(
            "UPDATE tool_run_completion_outbox
             SET delivered_at = datetime('now'), claimed_until = NULL
             WHERE tool_run_result_id = ?1 AND delivered_at IS NULL",
            rusqlite::params![tool_run_result_id],
        )?;
        Ok(changed > 0)
    }

    /// Acknowledge an unowned result only while both durable owner records are
    /// still empty. A late session binding runs as a SQLite writer too, so it
    /// either wins first and prevents this acknowledgement or reopens the row
    /// after an unowned acknowledgement wins.
    pub fn acknowledge_unowned_tool_run_completion(
        &self,
        tool_run_result_id: &str,
    ) -> anyhow::Result<bool> {
        let conn = self.conn();
        let changed = conn.execute(
            "UPDATE tool_run_completion_outbox
             SET delivered_at = datetime('now'), claimed_until = NULL
             WHERE tool_run_result_id = ?1
               AND delivered_at IS NULL
               AND session_id IS NULL
               AND EXISTS (
                   SELECT 1 FROM tool_runs
                   WHERE tool_runs.id = tool_run_completion_outbox.tool_run_id
                     AND tool_runs.kind = 'background'
                     AND tool_runs.session_id IS NULL
               )",
            rusqlite::params![tool_run_result_id],
        )?;
        Ok(changed > 0)
    }
}

use rusqlite::OptionalExtension;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Database;

    const STARTED: &str = "2026-09-24T10:00:00.000Z";
    const FINISHED: &str = "2026-09-24T11:00:00.000Z";
    const TOOL_RUN_ID: &str = "toolrun-d21abf676dbf16ce90950071af56a65c";

    #[test]
    fn terminal_background_tool_run_is_reconciled_and_acknowledged() {
        let db = Database::open_in_memory().unwrap();
        db.save_tool_run(
            TOOL_RUN_ID,
            Some("ses-7ae290dbf0b4468c9d2c401be649850b"),
            "echo ok",
            STARTED,
        )
        .unwrap();
        db.finish_tool_run(
            TOOL_RUN_ID,
            ToolRunStatus::Completed,
            Some("ok"),
            None,
            None,
            None,
            FINISHED,
        )
        .unwrap();
        let row = db.claim_tool_run_completion().unwrap().unwrap();
        assert_eq!(row.kind, "background");
        assert_eq!(row.payload.output.as_deref(), Some("ok"));
        assert!(
            db.acknowledge_tool_run_completion(&row.tool_run_result_id)
                .unwrap()
        );
        assert!(db.claim_tool_run_completion().unwrap().is_none());
    }

    #[test]
    fn unowned_completion_acknowledgement_requires_an_unowned_background_row() {
        let db = Database::open_in_memory().unwrap();
        db.save_tool_run(TOOL_RUN_ID, None, "echo ok", STARTED)
            .unwrap();
        db.finish_tool_run(
            TOOL_RUN_ID,
            ToolRunStatus::Failed,
            None,
            Some("failed"),
            Some("failed"),
            None,
            FINISHED,
        )
        .unwrap();
        db.reconcile_tool_run_completion_outbox().unwrap();
        assert!(
            db.acknowledge_unowned_tool_run_completion(TOOL_RUN_ID)
                .unwrap()
        );
        assert!(
            !db.acknowledge_unowned_tool_run_completion(TOOL_RUN_ID)
                .unwrap()
        );
    }
}
