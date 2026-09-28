//! Durable delivery records for terminal action results.
//!
//! The action row remains the source of the result payload. This repository
//! only records the delivery lifecycle so a transient completion broadcast can
//! be rebuilt and acknowledged after the owning session has durably projected
//! the result. Background actions and scheduled tool actions share this
//! delivery contract; scheduled firing and execution remain owned separately.

use crate::db::Database;
use haven_common::ActionStatus;
use haven_common::action_lease::ActionLease;
use serde_json::{Value, json};

const CLAIM_LEASE_SECS: i64 = 30;

#[derive(Debug, Clone)]
pub struct ActionCompletionOutboxRow {
    pub action_id: String,
    pub action_result_id: String,
    pub kind: String,
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
    pub(crate) fn reconcile_action_completion_outbox(&self) -> anyhow::Result<()> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT id, kind, mode, session_id, status, output, result_summary,
                    error, error_reason, log_path, exit_code, started_at, finished_at
             FROM actions
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
                row.get::<_, String>(4)?,
                row.get::<_, Option<String>>(5)?,
                row.get::<_, Option<String>>(6)?,
                row.get::<_, Option<String>>(7)?,
                row.get::<_, Option<String>>(8)?,
                row.get::<_, Option<String>>(9)?,
                row.get::<_, Option<i32>>(10)?,
                row.get::<_, Option<String>>(11)?,
                row.get::<_, Option<String>>(12)?,
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
                kind,
                mode,
                session_id,
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
                let status = ActionStatus::from_status_str(&status);
                let output = if kind == "scheduled" {
                    result_summary.as_deref()
                } else {
                    output.as_deref()
                };
                let status_json = serde_json::to_string(&status_json(
                    &action_id,
                    status,
                    output,
                    error.as_deref(),
                    error_reason.as_deref(),
                    log_path.as_deref(),
                    exit_code,
                    started_at.as_deref(),
                    finished_at.as_deref(),
                ))?;
                debug_assert!(kind != "scheduled" || mode == "tool");
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
                    "SELECT outbox.action_id, outbox.action_result_id, actions.kind,
                            outbox.session_id, outbox.status, outbox.status_json,
                            claimed_until, datetime('now')
                     FROM action_completion_outbox AS outbox
                     JOIN actions ON actions.id = outbox.action_id
                     WHERE outbox.delivered_at IS NULL
                       AND (outbox.claimed_until IS NULL OR outbox.claimed_until <= datetime('now'))
                     ORDER BY outbox.created_at ASC, outbox.action_id ASC
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
                action_id,
                action_result_id,
                kind,
                session_id,
                status,
                status_json,
                claimed_until,
                now,
            )) = row
            else {
                return Ok(None);
            };
            let current_lease = claimed_until
                .map(|expires_at| ActionLease::new(action_result_id.clone(), expires_at));
            if !ActionLease::can_claim(current_lease.as_ref(), &now) {
                return Ok(None);
            }
            conn.execute(
                "UPDATE action_completion_outbox
                 SET claimed_until = datetime('now', ?2)
                 WHERE action_id = ?1 AND delivered_at IS NULL",
                rusqlite::params![action_id, format!("+{CLAIM_LEASE_SECS} seconds")],
            )?;
            Ok(Some(ActionCompletionOutboxRow {
                action_id,
                action_result_id,
                kind,
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

    /// Acknowledge an unowned result only while both durable owner records are
    /// still empty. A late session binding runs as a SQLite writer too, so it
    /// either wins first and prevents this acknowledgement or reopens the row
    /// after an unowned acknowledgement wins.
    pub fn acknowledge_unowned_action_completion(
        &self,
        action_result_id: &str,
    ) -> anyhow::Result<bool> {
        let conn = self.conn();
        let changed = conn.execute(
            "UPDATE action_completion_outbox
             SET delivered_at = datetime('now'), claimed_until = NULL
             WHERE action_result_id = ?1
               AND delivered_at IS NULL
               AND session_id IS NULL
               AND EXISTS (
                   SELECT 1 FROM actions
                   WHERE actions.id = action_completion_outbox.action_id
                     AND (
                         actions.kind = 'background'
                         OR (actions.kind = 'scheduled' AND actions.mode = 'tool')
                     )
                     AND actions.session_id IS NULL
               )",
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

    fn save_scheduled_tool(db: &Database, id: &str, session_id: Option<&str>) {
        db.save_scheduled_action(
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
        assert!(db.start_scheduled_action(id, "started").unwrap());
    }

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
        assert_eq!(row.kind, "background");
        assert_eq!(row.session_id.as_deref(), Some("ses-1"));
        assert_eq!(row.status_json["output"], "ok");
        assert!(db.acknowledge_action_completion("act-outbox").unwrap());
        assert!(db.claim_action_completion().unwrap().is_none());
    }

    #[test]
    fn expired_completion_lease_can_be_reclaimed_and_ack_uses_result_identity() {
        let db = Database::open_in_memory().unwrap();
        db.save_action("act-expired-outbox", Some("ses-1"), "echo ok", "start")
            .unwrap();
        db.finish_action(
            "act-expired-outbox",
            ActionStatus::Completed,
            Some("ok"),
            None,
            None,
            None,
            Some(0),
            "finish",
        )
        .unwrap();

        let first = db.claim_action_completion().unwrap().unwrap();
        assert_eq!(first.action_result_id, "act-expired-outbox");
        assert!(db.claim_action_completion().unwrap().is_none());
        assert!(
            !db.acknowledge_action_completion("act-different-result")
                .unwrap()
        );

        db.conn()
            .execute(
                "UPDATE action_completion_outbox
                 SET claimed_until = datetime('now', '-1 second')
                 WHERE action_result_id = ?1",
                ["act-expired-outbox"],
            )
            .unwrap();
        let reclaimed = db.claim_action_completion().unwrap().unwrap();
        assert_eq!(reclaimed.action_result_id, first.action_result_id);
    }

    #[test]
    fn late_owner_binding_reopens_an_acknowledged_unowned_completion() {
        let db = Database::open_in_memory().unwrap();
        db.save_action("act-late-owner", None, "echo late", "start")
            .unwrap();
        db.finish_action(
            "act-late-owner",
            ActionStatus::Completed,
            Some("late output"),
            None,
            None,
            None,
            Some(0),
            "finish",
        )
        .unwrap();

        assert!(db.claim_action_completion().unwrap().is_some());
        assert!(db.acknowledge_action_completion("act-late-owner").unwrap());
        db.update_action_session("act-late-owner", "ses-late-owner")
            .unwrap();

        let completion = db
            .claim_action_completion()
            .unwrap()
            .expect("late binding must make the completion pending again");
        assert_eq!(completion.session_id.as_deref(), Some("ses-late-owner"));
        assert_eq!(completion.status_json["output"], "late output");
        assert!(!db.delete_action("act-late-owner").unwrap());
    }

    #[test]
    fn unowned_ack_is_rejected_after_owner_binding() {
        let db = Database::open_in_memory().unwrap();
        db.save_action("act-owner-wins", None, "echo owner", "start")
            .unwrap();
        db.finish_action(
            "act-owner-wins",
            ActionStatus::Completed,
            Some("owned output"),
            None,
            None,
            None,
            Some(0),
            "finish",
        )
        .unwrap();
        assert!(db.claim_action_completion().unwrap().is_some());

        db.update_action_session("act-owner-wins", "ses-owner-wins")
            .unwrap();
        assert!(
            !db.acknowledge_unowned_action_completion("act-owner-wins")
                .unwrap()
        );
        assert_eq!(
            db.claim_action_completion()
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
        db.save_action("act-cas", Some("ses-cas"), "echo winner", "started")
            .unwrap();

        assert!(db.finish_action_with_completion(
            "act-cas",
            ActionStatus::Completed,
            Some("winning output"),
            None,
            None,
            Some("winner.log"),
            Some(0),
            "winner finish",
            r#"{"action_id":"act-cas","status":"completed","output":"winning output","finished_at":"winner finish"}"#,
        )
        .unwrap());
        assert!(!db.finish_action_with_completion(
            "act-cas",
            ActionStatus::Failed,
            None,
            Some("late error"),
            Some("late reason"),
            Some("late.log"),
            Some(1),
            "late finish",
            r#"{"action_id":"act-cas","status":"failed","error":"late error","finished_at":"late finish"}"#,
        )
        .unwrap());

        let action = db.get_action("act-cas").unwrap().unwrap();
        assert_eq!(action.status, ActionStatus::Completed);
        assert_eq!(action.output.as_deref(), Some("winning output"));
        assert_eq!(action.log_path.as_deref(), Some("winner.log"));
        assert_eq!(action.finished_at.as_deref(), Some("winner finish"));

        let completion = db.claim_action_completion().unwrap().unwrap();
        assert_eq!(completion.status, action.status);
        assert_eq!(
            completion.session_id.as_deref(),
            action.session_id.as_deref()
        );
        assert_eq!(completion.status_json["status"], action.status.as_str());
        assert_eq!(
            completion.status_json["output"],
            action.output.as_deref().unwrap()
        );
        assert_eq!(
            completion.status_json["finished_at"],
            action.finished_at.as_deref().unwrap()
        );
        assert!(completion.status_json.get("error").is_none());
    }

    #[test]
    fn scheduled_tool_completed_and_failed_rows_use_the_same_outbox() {
        for (id, status, summary, error_reason) in [
            (
                "act-scheduled-completed",
                ActionStatus::Completed,
                Some("bounded success summary"),
                None,
            ),
            (
                "act-scheduled-failed",
                ActionStatus::Failed,
                None,
                Some("bounded failure summary"),
            ),
        ] {
            let db = Database::open_in_memory().unwrap();
            save_scheduled_tool(&db, id, Some("ses-scheduled"));
            assert!(
                db.finish_scheduled_action(id, status, summary, error_reason, "finished")
                    .unwrap()
            );

            let result = db.claim_action_completion().unwrap().unwrap();
            assert_eq!(result.action_id, id);
            assert_eq!(result.action_result_id, id);
            assert_eq!(result.kind, "scheduled");
            assert_eq!(result.session_id.as_deref(), Some("ses-scheduled"));
            assert_eq!(result.status, status);
            match status {
                ActionStatus::Completed => {
                    assert_eq!(result.status_json["output"], "bounded success summary");
                    assert!(result.status_json.get("error").is_none());
                }
                ActionStatus::Failed => {
                    assert_eq!(
                        result.status_json["error_reason"],
                        "bounded failure summary"
                    );
                    assert!(result.status_json.get("output").is_none());
                }
                ActionStatus::Waiting | ActionStatus::Running | ActionStatus::Cancelled => {
                    unreachable!()
                }
            }
            assert!(!db.delete_action(id).unwrap());
            assert!(db.acknowledge_action_completion(id).unwrap());
            assert!(db.claim_action_completion().unwrap().is_none());
        }
    }

    #[test]
    fn scheduled_tool_terminal_row_reconciles_after_a_lost_publish_or_restart() {
        let db = Database::open_in_memory().unwrap();
        save_scheduled_tool(&db, "act-scheduled-reconcile", Some("ses-reconcile"));
        // Simulate a terminal row written by an older writer or a crash window
        // before the completion outbox was populated.
        db.conn()
            .execute(
                "UPDATE actions SET status = 'failed', error_reason = ?2,
                    finished_at = 'finished' WHERE id = ?1",
                rusqlite::params!["act-scheduled-reconcile", "bounded failure summary"],
            )
            .unwrap();

        let first = db.claim_action_completion().unwrap().unwrap();
        assert_eq!(first.kind, "scheduled");
        assert_eq!(first.action_result_id, "act-scheduled-reconcile");
        assert_eq!(first.status, ActionStatus::Failed);
        assert_eq!(first.status_json["error_reason"], "bounded failure summary");
        assert!(db.claim_action_completion().unwrap().is_none());
        assert!(
            db.acknowledge_action_completion(&first.action_result_id)
                .unwrap()
        );
        assert!(db.claim_action_completion().unwrap().is_none());
    }

    #[test]
    fn unowned_scheduled_tool_is_ackable_and_cancelled_or_continue_rows_do_not_enqueue() {
        let db = Database::open_in_memory().unwrap();
        save_scheduled_tool(&db, "act-scheduled-unowned", None);
        assert!(
            db.finish_scheduled_action(
                "act-scheduled-unowned",
                ActionStatus::Completed,
                Some("summary"),
                None,
                "finished",
            )
            .unwrap()
        );
        let unowned = db.claim_action_completion().unwrap().unwrap();
        assert!(unowned.session_id.is_none());
        assert!(
            db.acknowledge_unowned_action_completion(&unowned.action_result_id)
                .unwrap()
        );
        assert!(db.claim_action_completion().unwrap().is_none());

        let cancelled = Database::open_in_memory().unwrap();
        cancelled
            .save_scheduled_action(
                "act-scheduled-cancelled",
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
                .cancel_scheduled_action("act-scheduled-cancelled", "cancelled")
                .unwrap()
        );
        assert!(cancelled.claim_action_completion().unwrap().is_none());

        let continue_mode = Database::open_in_memory().unwrap();
        continue_mode
            .save_scheduled_action(
                "act-scheduled-continue",
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
                .start_scheduled_action("act-scheduled-continue", "started")
                .unwrap()
        );
        assert!(
            continue_mode
                .finish_scheduled_action(
                    "act-scheduled-continue",
                    ActionStatus::Completed,
                    None,
                    None,
                    "finished",
                )
                .unwrap()
        );
        assert!(continue_mode.claim_action_completion().unwrap().is_none());
    }
}
