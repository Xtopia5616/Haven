//! Durable delivery records for terminal background-action results.
//!
//! The action row remains the source of the result payload. This repository
//! only records the delivery lifecycle so a transient completion broadcast can
//! be rebuilt and acknowledged after the owning session has durably projected
//! the result.

use crate::db::Database;
use haven_common::ActionStatus;
use serde_json::{Value, json};

const CLAIM_LEASE_SECS: i64 = 30;

#[derive(Debug, Clone)]
pub struct ActionCompletionOutboxRow {
    pub action_id: String,
    pub action_result_id: String,
    pub session_id: Option<String>,
    pub status: ActionStatus,
    pub status_json: Value,
}

#[allow(clippy::too_many_arguments)]
fn status_json(
    action_id: &str,
    status: ActionStatus,
    output: Option<&str>,
    error: Option<&str>,
    error_reason: Option<&str>,
    log_path: Option<&str>,
    exit_code: Option<i32>,
    started_at: Option<&str>,
    finished_at: Option<&str>,
) -> Value {
    let mut value = json!({
        "action_id": action_id,
        "status": status.as_str(),
    });
    if let Some(output) = output {
        value["output"] = json!(output);
    }
    if let Some(error) = error {
        value["error"] = json!(error);
    }
    if let Some(error_reason) = error_reason {
        value["error_reason"] = json!(error_reason);
    }
    if let Some(log_path) = log_path {
        value["log_path"] = json!(log_path);
    }
    if let Some(exit_code) = exit_code {
        value["exit_code"] = json!(exit_code);
    }
    if let Some(started_at) = started_at {
        value["started_at"] = json!(started_at);
    }
    if let Some(finished_at) = finished_at {
        value["finished_at"] = json!(finished_at);
    }
    value
}

impl Database {
    /// Rebuild missing outbox rows from terminal action history. This is the
    /// crash-recovery half of the outbox: a process can die after the action
    /// row commits but before the completion record is inserted.
    fn reconcile_action_completion_outbox(&self) -> anyhow::Result<()> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT id, session_id, status, output, error, error_reason, log_path,
                    exit_code, started_at, finished_at
             FROM actions
             WHERE kind = 'background' AND status IN ('completed', 'failed')
             ORDER BY created_at ASC, id ASC",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, Option<String>>(5)?,
                row.get::<_, Option<String>>(6)?,
                row.get::<_, Option<i32>>(7)?,
                row.get::<_, Option<String>>(8)?,
                row.get::<_, Option<String>>(9)?,
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
                action_id,
                session_id,
                status,
                output,
                error,
                error_reason,
                log_path,
                exit_code,
                started_at,
                finished_at,
            ) in terminal
            {
                let status = ActionStatus::from_status_str(&status);
                let status_json = serde_json::to_string(&status_json(
                    &action_id,
                    status,
                    output.as_deref(),
                    error.as_deref(),
                    error_reason.as_deref(),
                    log_path.as_deref(),
                    exit_code,
                    started_at.as_deref(),
                    finished_at.as_deref(),
                ))?;
                conn.execute(
                    "INSERT OR IGNORE INTO action_completion_outbox
                         (action_id, action_result_id, session_id, status, status_json)
                     VALUES (?1, ?1, ?2, ?3, ?4)",
                    rusqlite::params![action_id, session_id, status.as_str(), status_json],
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
    pub fn claim_action_completion(&self) -> anyhow::Result<Option<ActionCompletionOutboxRow>> {
        self.reconcile_action_completion_outbox()?;
        let conn = self.conn();
        conn.execute_batch("BEGIN IMMEDIATE")?;
        let result = (|| {
            let row = conn
                .query_row(
                    "SELECT action_id, action_result_id, session_id, status, status_json
                     FROM action_completion_outbox
                     WHERE delivered_at IS NULL
                       AND (claimed_until IS NULL OR claimed_until <= datetime('now'))
                     ORDER BY created_at ASC, action_id ASC
                     LIMIT 1",
                    [],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, Option<String>>(2)?,
                            row.get::<_, String>(3)?,
                            row.get::<_, String>(4)?,
                        ))
                    },
                )
                .optional()?;
            let Some((action_id, action_result_id, session_id, status, status_json)) = row else {
                return Ok(None);
            };
            conn.execute(
                "UPDATE action_completion_outbox
                 SET claimed_until = datetime('now', ?2)
                 WHERE action_id = ?1 AND delivered_at IS NULL",
                rusqlite::params![action_id, format!("+{CLAIM_LEASE_SECS} seconds")],
            )?;
            Ok(Some(ActionCompletionOutboxRow {
                action_id,
                action_result_id,
                session_id,
                status: ActionStatus::from_status_str(&status),
                status_json: serde_json::from_str(&status_json)?,
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
    pub fn acknowledge_action_completion(&self, action_result_id: &str) -> anyhow::Result<bool> {
        let conn = self.conn();
        let changed = conn.execute(
            "UPDATE action_completion_outbox
             SET delivered_at = datetime('now'), claimed_until = NULL
             WHERE action_result_id = ?1 AND delivered_at IS NULL",
            rusqlite::params![action_result_id],
        )?;
        Ok(changed > 0)
    }
}

use rusqlite::OptionalExtension;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Database;

    #[test]
    fn terminal_action_is_reconciled_and_acknowledged() {
        let db = Database::open_in_memory().unwrap();
        db.save_action("act-outbox", Some("ses-1"), "echo ok", "start")
            .unwrap();
        db.finish_action(
            "act-outbox",
            ActionStatus::Completed,
            Some("ok"),
            None,
            None,
            None,
            Some(0),
            "finish",
        )
        .unwrap();

        let row = db.claim_action_completion().unwrap().unwrap();
        assert_eq!(row.action_id, "act-outbox");
        assert_eq!(row.session_id.as_deref(), Some("ses-1"));
        assert_eq!(row.status_json["output"], "ok");
        assert!(db.acknowledge_action_completion("act-outbox").unwrap());
        assert!(db.claim_action_completion().unwrap().is_none());
    }
}
