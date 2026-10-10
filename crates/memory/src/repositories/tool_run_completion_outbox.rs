//! Durable delivery records for terminal ToolRun results.
//!
//! The ToolRun row remains the source of the result payload. This repository
//! only records the delivery lifecycle so a transient completion broadcast can
//! be rebuilt and acknowledged after the owning session has durably projected
//! the result. Background and scheduled ToolRuns share this
//! delivery contract; scheduled firing and execution remain owned separately.

use crate::db::Database;
use haven_common::tool_run_lease::ToolRunLease;
use haven_common::{ToolRunCompletionPayload, ToolRunStatus};

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
    status: ToolRunStatus,
    output: Option<&str>,
    error: Option<&str>,
    error_reason: Option<&str>,
    log_path: Option<&str>,
    exit_code: Option<i32>,
    started_at: Option<&str>,
    finished_at: Option<&str>,
    source_step_id: Option<&str>,
) -> ToolRunCompletionPayload {
    ToolRunCompletionPayload {
        tool_run_id: tool_run_id.to_owned(),
        status,
        status_projection_kind: None,
        output: output.map(str::to_owned),
        error: error.map(str::to_owned),
        error_reason: error_reason.map(str::to_owned),
        log_path: log_path.map(str::to_owned),
        exit_code,
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
            "SELECT id, kind, mode, session_id, source_step_id, status, output, result_summary,
                    error, error_reason, log_path, exit_code, started_at, finished_at
             FROM tool_runs
             WHERE (kind = 'background' OR (kind = 'scheduled' AND mode = 'tool'))
               AND status IN ('completed', 'failed')
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
                row.get::<_, Option<i32>>(11)?,
                row.get::<_, Option<String>>(12)?,
                row.get::<_, Option<String>>(13)?,
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
                mode,
                session_id,
                source_step_id,
                status,
                output,
                result_summary,
                error,
                error_reason,
                log_path,
                exit_code,
                started_at,
                finished_at,
            ) in terminal
            {
                let status = ToolRunStatus::from_status_str(&status);
                let output = if kind == "scheduled" {
                    result_summary.as_deref()
                } else {
                    output.as_deref()
                };
                let payload = serde_json::to_string(&completion_payload(
                    &tool_run_id,
                    status,
                    output,
                    error.as_deref(),
                    error_reason.as_deref(),
                    log_path.as_deref(),
                    exit_code,
                    started_at.as_deref(),
                    finished_at.as_deref(),
                    source_step_id.as_deref(),
                ))?;
                debug_assert!(kind != "scheduled" || mode == "tool");
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
                            row.get::<_, Option<String>>(3)?,
                            row.get::<_, String>(4)?,
                            row.get::<_, String>(5)?,
                            row.get::<_, Option<String>>(6)?,
                            row.get::<_, String>(7)?,
                        ))
                    },
                )
                .optional()?;
            let Some((
                tool_run_id,
                tool_run_result_id,
                kind,
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
            Ok(Some(ToolRunCompletionOutboxRow {
                tool_run_id,
                tool_run_result_id,
                kind,
                session_id,
                status: ToolRunStatus::from_status_str(&status),
                payload: serde_json::from_str(&payload_json)?,
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
                     AND (
                         tool_runs.kind = 'background'
                         OR (tool_runs.kind = 'scheduled' AND tool_runs.mode = 'tool')
                     )
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

    fn save_scheduled_tool(db: &Database, id: &str, session_id: Option<&str>) {
        db.save_scheduled_tool_run(
            id,
            "2026-09-29T12:00:00Z",
            "Scheduled tool",
            "Call the tool",
            "tool",
            session_id,
            Some("notify"),
            Some(r#"{"title":"hello","body":"world"}"#),
            None,
            None,
        )
        .unwrap();
        assert!(db.start_scheduled_tool_run(id, "started").unwrap());
    }

    #[test]
    fn terminal_tool_run_is_reconciled_and_acknowledged() {
        let db = Database::open_in_memory().unwrap();
        db.save_tool_run("toolrun-outbox", Some("ses-1"), "echo ok", "start")
            .unwrap();
        db.finish_tool_run(
            "toolrun-outbox",
            ToolRunStatus::Completed,
            Some("ok"),
            None,
            None,
            None,
            Some(0),
            "finish",
        )
        .unwrap();

        let row = db.claim_tool_run_completion().unwrap().unwrap();
        assert_eq!(row.tool_run_id, "toolrun-outbox");
        assert_eq!(row.kind, "background");
        assert_eq!(row.session_id.as_deref(), Some("ses-1"));
        assert_eq!(row.payload.output.as_deref(), Some("ok"));
        assert!(
            db.acknowledge_tool_run_completion("toolrun-outbox")
                .unwrap()
        );
        assert!(db.claim_tool_run_completion().unwrap().is_none());
    }

    #[test]
    fn reconciled_background_completion_retains_source_step() {
        let db = Database::open_in_memory().unwrap();
        db.save_tool_run_with_source(
            "toolrun-source-reconcile",
            Some("ses-source-reconcile"),
            "echo source",
            "start",
            Some("step-source-reconcile"),
        )
        .unwrap();
        db.finish_tool_run(
            "toolrun-source-reconcile",
            ToolRunStatus::Completed,
            Some("done"),
            None,
            None,
            None,
            Some(0),
            "finish",
        )
        .unwrap();

        let completion = db.claim_tool_run_completion().unwrap().unwrap();
        assert_eq!(
            completion.payload.source_step_id.as_deref(),
            Some("step-source-reconcile")
        );
    }

    #[test]
    fn expired_completion_lease_can_be_reclaimed_and_ack_uses_result_identity() {
        let db = Database::open_in_memory().unwrap();
        db.save_tool_run("toolrun-expired-outbox", Some("ses-1"), "echo ok", "start")
            .unwrap();
        db.finish_tool_run(
            "toolrun-expired-outbox",
            ToolRunStatus::Completed,
            Some("ok"),
            None,
            None,
            None,
            Some(0),
            "finish",
        )
        .unwrap();

        let first = db.claim_tool_run_completion().unwrap().unwrap();
        assert_eq!(first.tool_run_result_id, "toolrun-expired-outbox");
        assert!(db.claim_tool_run_completion().unwrap().is_none());
        assert!(
            !db.acknowledge_tool_run_completion("toolrun-different-result")
                .unwrap()
        );

        db.conn()
            .execute(
                "UPDATE tool_run_completion_outbox
                 SET claimed_until = datetime('now', '-1 second')
                 WHERE tool_run_result_id = ?1",
                ["toolrun-expired-outbox"],
            )
            .unwrap();
        let reclaimed = db.claim_tool_run_completion().unwrap().unwrap();
        assert_eq!(reclaimed.tool_run_result_id, first.tool_run_result_id);
    }

    #[test]
    fn late_owner_binding_reopens_an_acknowledged_unowned_completion() {
        let db = Database::open_in_memory().unwrap();
        db.save_tool_run("toolrun-late-owner", None, "echo late", "start")
            .unwrap();
        db.finish_tool_run(
            "toolrun-late-owner",
            ToolRunStatus::Completed,
            Some("late output"),
            None,
            None,
            None,
            Some(0),
            "finish",
        )
        .unwrap();

        assert!(db.claim_tool_run_completion().unwrap().is_some());
        assert!(
            db.acknowledge_tool_run_completion("toolrun-late-owner")
                .unwrap()
        );
        db.update_tool_run_session("toolrun-late-owner", "ses-late-owner")
            .unwrap();

        let completion = db
            .claim_tool_run_completion()
            .unwrap()
            .expect("late binding must make the completion pending again");
        assert_eq!(completion.session_id.as_deref(), Some("ses-late-owner"));
        assert_eq!(completion.payload.output.as_deref(), Some("late output"));
        assert!(!db.delete_tool_run("toolrun-late-owner").unwrap());
    }

    #[test]
    fn unowned_ack_is_rejected_after_owner_binding() {
        let db = Database::open_in_memory().unwrap();
        db.save_tool_run("toolrun-owner-wins", None, "echo owner", "start")
            .unwrap();
        db.finish_tool_run(
            "toolrun-owner-wins",
            ToolRunStatus::Completed,
            Some("owned output"),
            None,
            None,
            None,
            Some(0),
            "finish",
        )
        .unwrap();
        assert!(db.claim_tool_run_completion().unwrap().is_some());

        db.update_tool_run_session("toolrun-owner-wins", "ses-owner-wins")
            .unwrap();
        assert!(
            !db.acknowledge_unowned_tool_run_completion("toolrun-owner-wins")
                .unwrap()
        );
        assert_eq!(
            db.claim_tool_run_completion()
                .unwrap()
                .unwrap()
                .session_id
                .as_deref(),
            Some("ses-owner-wins")
        );
    }

    #[test]
    fn completion_snapshot_belongs_to_the_winning_terminal_transition() {
        let db = Database::open_in_memory().unwrap();
        db.save_tool_run("toolrun-cas", Some("ses-cas"), "echo winner", "started")
            .unwrap();

        let winning_payload = completion_payload(
            "toolrun-cas",
            ToolRunStatus::Completed,
            Some("winning output"),
            None,
            None,
            Some("winner.log"),
            Some(0),
            Some("started"),
            Some("winner finish"),
            None,
        );
        assert!(
            db.finish_tool_run_with_completion(&winning_payload)
                .unwrap()
        );
        let losing_payload = completion_payload(
            "toolrun-cas",
            ToolRunStatus::Failed,
            None,
            Some("late error"),
            Some("late reason"),
            Some("late.log"),
            Some(1),
            Some("started"),
            Some("late finish"),
            None,
        );
        assert!(!db.finish_tool_run_with_completion(&losing_payload).unwrap());

        let tool_run = db.get_tool_run("toolrun-cas").unwrap().unwrap();
        assert_eq!(tool_run.status, ToolRunStatus::Completed);
        assert_eq!(tool_run.output.as_deref(), Some("winning output"));
        assert_eq!(tool_run.log_path.as_deref(), Some("winner.log"));
        assert_eq!(tool_run.finished_at.as_deref(), Some("winner finish"));

        let completion = db.claim_tool_run_completion().unwrap().unwrap();
        assert_eq!(completion.status, tool_run.status);
        assert_eq!(
            completion.session_id.as_deref(),
            tool_run.session_id.as_deref()
        );
        assert_eq!(completion.payload.status, tool_run.status);
        assert_eq!(
            completion.payload.output.as_deref(),
            tool_run.output.as_deref()
        );
        assert_eq!(
            completion.payload.finished_at.as_deref(),
            tool_run.finished_at.as_deref()
        );
        assert!(completion.payload.error.is_none());
    }

    #[test]
    fn scheduled_tool_completed_and_failed_rows_use_the_same_outbox() {
        for (id, status, summary, error_reason) in [
            (
                "toolrun-scheduled-completed",
                ToolRunStatus::Completed,
                Some("bounded success summary"),
                None,
            ),
            (
                "toolrun-scheduled-failed",
                ToolRunStatus::Failed,
                None,
                Some("bounded failure summary"),
            ),
        ] {
            let db = Database::open_in_memory().unwrap();
            save_scheduled_tool(&db, id, Some("ses-scheduled"));
            assert!(
                db.finish_scheduled_tool_run(id, status, summary, error_reason, "finished")
                    .unwrap()
            );

            let result = db.claim_tool_run_completion().unwrap().unwrap();
            assert_eq!(result.tool_run_id, id);
            assert_eq!(result.tool_run_result_id, id);
            assert_eq!(result.kind, "scheduled");
            assert_eq!(result.session_id.as_deref(), Some("ses-scheduled"));
            assert_eq!(result.status, status);
            match status {
                ToolRunStatus::Completed => {
                    assert_eq!(
                        result.payload.output.as_deref(),
                        Some("bounded success summary")
                    );
                    assert!(result.payload.error.is_none());
                }
                ToolRunStatus::Failed => {
                    assert_eq!(
                        result.payload.error_reason.as_deref(),
                        Some("bounded failure summary")
                    );
                    assert!(result.payload.output.is_none());
                }
                ToolRunStatus::Waiting | ToolRunStatus::Running | ToolRunStatus::Cancelled => {
                    unreachable!()
                }
            }
            assert!(!db.delete_tool_run(id).unwrap());
            assert!(db.acknowledge_tool_run_completion(id).unwrap());
            assert!(db.claim_tool_run_completion().unwrap().is_none());
        }
    }

    #[test]
    fn scheduled_tool_terminal_row_reconciles_after_a_lost_publish_or_restart() {
        let db = Database::open_in_memory().unwrap();
        save_scheduled_tool(&db, "toolrun-scheduled-reconcile", Some("ses-reconcile"));
        // Simulate a terminal row written by an older writer or a crash window
        // before the completion outbox was populated.
        db.conn()
            .execute(
                "UPDATE tool_runs SET status = 'failed', error_reason = ?2,
                    finished_at = 'finished' WHERE id = ?1",
                rusqlite::params!["toolrun-scheduled-reconcile", "bounded failure summary"],
            )
            .unwrap();

        let first = db.claim_tool_run_completion().unwrap().unwrap();
        assert_eq!(first.kind, "scheduled");
        assert_eq!(first.tool_run_result_id, "toolrun-scheduled-reconcile");
        assert_eq!(first.status, ToolRunStatus::Failed);
        assert_eq!(
            first.payload.error_reason.as_deref(),
            Some("bounded failure summary")
        );
        assert!(db.claim_tool_run_completion().unwrap().is_none());
        assert!(
            db.acknowledge_tool_run_completion(&first.tool_run_result_id)
                .unwrap()
        );
        assert!(db.claim_tool_run_completion().unwrap().is_none());
    }

    #[test]
    fn unowned_scheduled_tool_is_ackable_and_cancelled_or_continue_rows_do_not_enqueue() {
        let db = Database::open_in_memory().unwrap();
        save_scheduled_tool(&db, "toolrun-scheduled-unowned", None);
        assert!(
            db.finish_scheduled_tool_run(
                "toolrun-scheduled-unowned",
                ToolRunStatus::Completed,
                Some("summary"),
                None,
                "finished",
            )
            .unwrap()
        );
        let unowned = db.claim_tool_run_completion().unwrap().unwrap();
        assert!(unowned.session_id.is_none());
        assert!(
            db.acknowledge_unowned_tool_run_completion(&unowned.tool_run_result_id)
                .unwrap()
        );
        assert!(db.claim_tool_run_completion().unwrap().is_none());

        let cancelled = Database::open_in_memory().unwrap();
        cancelled
            .save_scheduled_tool_run(
                "toolrun-scheduled-cancelled",
                "2026-09-29T12:00:00Z",
                "Scheduled tool",
                "Call the tool",
                "tool",
                Some("ses-cancelled"),
                Some("notify"),
                Some("{}"),
                None,
                None,
            )
            .unwrap();
        assert!(
            cancelled
                .cancel_scheduled_tool_run("toolrun-scheduled-cancelled", "cancelled")
                .unwrap()
        );
        assert!(cancelled.claim_tool_run_completion().unwrap().is_none());

        let continue_mode = Database::open_in_memory().unwrap();
        continue_mode
            .save_scheduled_tool_run(
                "toolrun-scheduled-continue",
                "2026-09-29T12:00:00Z",
                "Scheduled continue",
                "Continue the session",
                "continue",
                Some("ses-continue"),
                None,
                None,
                Some("continue"),
                None,
            )
            .unwrap();
        assert!(
            continue_mode
                .start_scheduled_tool_run("toolrun-scheduled-continue", "started")
                .unwrap()
        );
        assert!(
            continue_mode
                .finish_scheduled_tool_run(
                    "toolrun-scheduled-continue",
                    ToolRunStatus::Completed,
                    None,
                    None,
                    "finished",
                )
                .unwrap()
        );
        assert!(continue_mode.claim_tool_run_completion().unwrap().is_none());
    }
}
