//! Durable recurring tasks that start a fresh Agent session at each interval.
//!
//! ToolRun remains the owner of each individual occurrence. This table stores
//! only the recurring schedule and the identity of its currently active
//! occurrence.

use crate::db::Database;
use crate::repositories::messages::now_rfc3339_millis;
use crate::repositories::tool_runs::ScheduledToolRunRow;
use anyhow::Context;
use chrono::{DateTime, SecondsFormat, Utc};
use haven_common::lifecycle::ToolRunStatus;
use haven_common::types::{is_canonical_id, new_id};
use rusqlite::OptionalExtension;

pub const MIN_RECURRING_SESSION_INTERVAL_SECS: u64 = 60;
pub const MAX_RECURRING_SESSION_INTERVAL_SECS: u64 = 31_536_000;
const MAX_RECURRING_SESSION_PROMPT_BYTES: usize = 64 * 1024;
const MAX_RECURRING_SESSION_TASKS: usize = 64;
const MAX_RECURRING_SESSION_TASK_CLAIMS_PER_POLL: usize = 32;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RecurringSessionTaskRow {
    pub schedule_id: String,
    pub source_session_id: String,
    pub title: String,
    pub body: String,
    pub interval_seconds: u64,
    pub anchor_at: String,
    pub next_due_at: String,
    pub enabled: bool,
    pub active_tool_run_id: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

fn canonical_timestamp(value: &str, field: &str) -> anyhow::Result<String> {
    let parsed = DateTime::parse_from_rfc3339(value)
        .with_context(|| format!("{field} must be an RFC3339 timestamp"))?;
    Ok(parsed
        .with_timezone(&Utc)
        .to_rfc3339_opts(SecondsFormat::Millis, true))
}

fn validate_task(
    source_session_id: &str,
    title: &str,
    body: &str,
    prompt: &str,
    interval_seconds: u64,
) -> anyhow::Result<()> {
    anyhow::ensure!(
        is_canonical_id(source_session_id, "ses"),
        "invalid source_session_id"
    );
    anyhow::ensure!(
        !title.trim().is_empty() && title.chars().count() <= 80,
        "scheduled session title must contain 1 to 80 characters"
    );
    anyhow::ensure!(
        !body.trim().is_empty() && body.chars().count() <= 2000,
        "scheduled session body must contain 1 to 2000 characters"
    );
    anyhow::ensure!(
        !prompt.trim().is_empty() && prompt.len() <= MAX_RECURRING_SESSION_PROMPT_BYTES,
        "scheduled session prompt is empty or exceeds its size limit"
    );
    anyhow::ensure!(
        (MIN_RECURRING_SESSION_INTERVAL_SECS..=MAX_RECURRING_SESSION_INTERVAL_SECS)
            .contains(&interval_seconds),
        "recurring interval must be between 60 seconds and 1 year"
    );
    Ok(())
}

fn row_from_sql(row: &rusqlite::Row<'_>) -> rusqlite::Result<RecurringSessionTaskRow> {
    Ok(RecurringSessionTaskRow {
        schedule_id: row.get(0)?,
        source_session_id: row.get(1)?,
        title: row.get(2)?,
        body: row.get(3)?,
        interval_seconds: u64::try_from(row.get::<_, i64>(4)?).unwrap_or_default(),
        anchor_at: row.get(5)?,
        next_due_at: row.get(6)?,
        enabled: row.get::<_, i64>(7)? == 1,
        active_tool_run_id: row.get(8)?,
        created_at: row.get(9)?,
        updated_at: row.get(10)?,
    })
}

fn validate_row(row: &RecurringSessionTaskRow) -> anyhow::Result<()> {
    anyhow::ensure!(
        is_canonical_id(&row.schedule_id, "sched"),
        "invalid schedule_id"
    );
    validate_task(
        &row.source_session_id,
        &row.title,
        &row.body,
        "stored prompt is omitted from schedule views",
        row.interval_seconds,
    )?;
    anyhow::ensure!(
        row.active_tool_run_id
            .as_deref()
            .is_none_or(|id| is_canonical_id(id, "toolrun")),
        "invalid active_tool_run_id"
    );
    Ok(())
}

impl Database {
    pub fn create_recurring_session_task(
        &self,
        source_session_id: &str,
        title: &str,
        body: &str,
        prompt: &str,
        interval_seconds: u64,
        anchor_at: &str,
    ) -> anyhow::Result<RecurringSessionTaskRow> {
        validate_task(source_session_id, title, body, prompt, interval_seconds)?;
        let anchor_at = canonical_timestamp(anchor_at, "anchor_at")?;
        let schedule_id = new_id("sched");
        let source_session_id = source_session_id.to_owned();
        let title = title.trim().to_owned();
        let body = body.trim().to_owned();
        let prompt = prompt.trim().to_owned();
        let interval_seconds = i64::try_from(interval_seconds).context("interval overflows")?;
        let now = now_rfc3339_millis();
        let conn = self.conn();
        conn.execute_batch("BEGIN IMMEDIATE")?;
        let result = (|| -> anyhow::Result<RecurringSessionTaskRow> {
            let count: i64 = conn.query_row(
                "SELECT COUNT(*) FROM recurring_session_tasks WHERE enabled = 1",
                [],
                |row| row.get(0),
            )?;
            anyhow::ensure!(
                usize::try_from(count).unwrap_or(usize::MAX) < MAX_RECURRING_SESSION_TASKS,
                "too many recurring session tasks are enabled"
            );
            conn.execute(
                "INSERT INTO recurring_session_tasks
                    (id, source_session_id, title, body, prompt, interval_seconds, anchor_at,
                     next_due_at, enabled, active_tool_run_id, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7, 1, NULL, ?8, ?8)",
                rusqlite::params![
                    schedule_id,
                    source_session_id,
                    title,
                    body,
                    prompt,
                    interval_seconds,
                    anchor_at,
                    now
                ],
            )?;
            let row = conn.query_row(
                "SELECT id, source_session_id, title, body, interval_seconds, anchor_at, next_due_at,
                        enabled, active_tool_run_id, created_at, updated_at
                 FROM recurring_session_tasks WHERE id = ?1",
                [&schedule_id],
                row_from_sql,
            )?;
            validate_row(&row)?;
            Ok(row)
        })();
        match result {
            Ok(row) => {
                conn.execute_batch("COMMIT")?;
                Ok(row)
            }
            Err(error) => {
                let _ = conn.execute_batch("ROLLBACK");
                Err(error)
            }
        }
    }

    pub fn list_recurring_session_tasks(
        &self,
        source_session_id: &str,
    ) -> anyhow::Result<Vec<RecurringSessionTaskRow>> {
        anyhow::ensure!(
            is_canonical_id(source_session_id, "ses"),
            "invalid session_id"
        );
        let conn = self.conn();
        let mut statement = conn.prepare(
            "SELECT id, source_session_id, title, body, interval_seconds, anchor_at, next_due_at,
                    enabled, active_tool_run_id, created_at, updated_at
             FROM recurring_session_tasks WHERE source_session_id = ?1
             ORDER BY enabled DESC, next_due_at, title COLLATE NOCASE",
        )?;
        let rows = statement.query_map([source_session_id], row_from_sql)?;
        let mut schedules = Vec::new();
        for row in rows {
            let mut row = row?;
            row.anchor_at = canonical_timestamp(&row.anchor_at, "anchor_at")?;
            row.next_due_at = canonical_timestamp(&row.next_due_at, "next_due_at")?;
            row.created_at = canonical_timestamp(&row.created_at, "created_at")?;
            row.updated_at = canonical_timestamp(&row.updated_at, "updated_at")?;
            validate_row(&row)?;
            schedules.push(row);
        }
        Ok(schedules)
    }

    pub fn delete_recurring_session_task(
        &self,
        schedule_id: &str,
        source_session_id: &str,
    ) -> anyhow::Result<(bool, Option<String>)> {
        anyhow::ensure!(is_canonical_id(schedule_id, "sched"), "invalid schedule_id");
        anyhow::ensure!(
            is_canonical_id(source_session_id, "ses"),
            "invalid session_id"
        );
        let conn = self.conn();
        conn.execute_batch("BEGIN IMMEDIATE")?;
        let result = (|| -> anyhow::Result<(bool, Option<String>)> {
            let active_tool_run_id = conn
                .query_row(
                    "SELECT active_tool_run_id FROM recurring_session_tasks
                     WHERE id = ?1 AND source_session_id = ?2",
                    rusqlite::params![schedule_id, source_session_id],
                    |row| row.get(0),
                )
                .optional()?;
            let Some(active_tool_run_id) = active_tool_run_id else {
                return Ok((false, None));
            };
            let deleted = conn.execute(
                "DELETE FROM recurring_session_tasks WHERE id = ?1 AND source_session_id = ?2",
                rusqlite::params![schedule_id, source_session_id],
            )?;
            Ok((deleted > 0, active_tool_run_id))
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

    /// Atomically create one standard scheduled ToolRun occurrence for each
    /// due recurring task. The next due timestamp advances only when the
    /// occurrence reaches a terminal state, so a task never overlaps itself.
    pub fn claim_due_recurring_session_tasks(
        &self,
        now: &str,
        limit: usize,
    ) -> anyhow::Result<Vec<ScheduledToolRunRow>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let now = canonical_timestamp(now, "now")?;
        let limit = i64::try_from(limit.min(MAX_RECURRING_SESSION_TASK_CLAIMS_PER_POLL))?;
        let conn = self.conn();
        conn.execute_batch("BEGIN IMMEDIATE")?;
        let result = (|| -> anyhow::Result<Vec<ScheduledToolRunRow>> {
            let due_tasks = {
                let mut statement = conn.prepare(
                    "SELECT id, source_session_id, title, body, prompt, interval_seconds,
                            anchor_at, next_due_at
                     FROM recurring_session_tasks
                     WHERE enabled = 1 AND active_tool_run_id IS NULL AND next_due_at <= ?1
                     ORDER BY next_due_at, id LIMIT ?2",
                )?;
                let rows = statement.query_map(rusqlite::params![&now, limit], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, i64>(5)?,
                        row.get::<_, String>(6)?,
                        row.get::<_, String>(7)?,
                    ))
                })?;
                rows.collect::<Result<Vec<_>, _>>()?
            };
            let mut occurrences = Vec::with_capacity(due_tasks.len());
            for (schedule_id, source_session_id, title, body, prompt, interval, anchor, due_at) in
                due_tasks
            {
                anyhow::ensure!(
                    is_canonical_id(&schedule_id, "sched"),
                    "invalid schedule_id"
                );
                anyhow::ensure!(
                    is_canonical_id(&source_session_id, "ses"),
                    "invalid source_session_id"
                );
                let interval_seconds =
                    u64::try_from(interval).context("invalid recurring interval")?;
                validate_task(&source_session_id, &title, &body, &prompt, interval_seconds)?;
                let due_at = canonical_timestamp(&due_at, "next_due_at")?;
                let anchor_at = canonical_timestamp(&anchor, "anchor_at")?;
                let now_value = DateTime::parse_from_rfc3339(&now)?.with_timezone(&Utc);
                let due_value = DateTime::parse_from_rfc3339(&due_at)?.with_timezone(&Utc);
                let anchor_value = DateTime::parse_from_rfc3339(&anchor_at)?.with_timezone(&Utc);
                anyhow::ensure!(
                    anchor_value <= due_value && due_value <= now_value,
                    "recurring task timing is invalid"
                );

                let tool_run_id = new_id("toolrun");
                conn.execute(
                    "INSERT INTO tool_runs
                        (id, kind, due_at, title, body, mode, session_id, prompt, status, created_at)
                     VALUES (?1, 'scheduled', ?2, ?3, ?4, 'new_session', ?5, ?6, 'waiting', ?7)",
                    rusqlite::params![tool_run_id, due_at, title, body, source_session_id, prompt, now],
                )?;
                let changed = conn.execute(
                    "UPDATE recurring_session_tasks SET active_tool_run_id = ?2, updated_at = ?3
                     WHERE id = ?1 AND enabled = 1 AND active_tool_run_id IS NULL",
                    rusqlite::params![schedule_id, tool_run_id, now],
                )?;
                anyhow::ensure!(changed == 1, "recurring session task claim was lost");
                occurrences.push(ScheduledToolRunRow {
                    tool_run_id,
                    due_at,
                    title: title.clone(),
                    body,
                    mode: "new_session".into(),
                    session_id: Some(source_session_id),
                    tool_name: None,
                    tool_args: None,
                    prompt: Some(prompt),
                    watch_tool_run_id: None,
                    status: ToolRunStatus::Waiting,
                    created_at: now.clone(),
                });
            }
            Ok(occurrences)
        })();
        match result {
            Ok(occurrences) => {
                conn.execute_batch("COMMIT")?;
                Ok(occurrences)
            }
            Err(error) => {
                let _ = conn.execute_batch("ROLLBACK");
                Err(error)
            }
        }
    }
}

/// Advance the recurring task after its active ToolRun completes. Missed ticks
/// are skipped, keeping the schedule on its original UTC interval anchor.
pub(super) fn settle_recurring_session_task(
    conn: &rusqlite::Connection,
    tool_run_id: &str,
    finished_at: &str,
) -> anyhow::Result<()> {
    let task: Option<(String, String, i64)> = conn
        .query_row(
            "SELECT id, anchor_at, interval_seconds
             FROM recurring_session_tasks WHERE active_tool_run_id = ?1 AND enabled = 1",
            [tool_run_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    let Some((schedule_id, anchor_at, interval_seconds)) = task else {
        return Ok(());
    };
    let interval_seconds = u64::try_from(interval_seconds).context("invalid recurring interval")?;
    anyhow::ensure!(
        (MIN_RECURRING_SESSION_INTERVAL_SECS..=MAX_RECURRING_SESSION_INTERVAL_SECS)
            .contains(&interval_seconds),
        "invalid recurring session interval"
    );
    let anchor = DateTime::parse_from_rfc3339(&anchor_at)?.with_timezone(&Utc);
    let finished = DateTime::parse_from_rfc3339(finished_at)?.with_timezone(&Utc);
    let interval_seconds =
        i64::try_from(interval_seconds).context("recurring interval overflows")?;
    let elapsed = finished.signed_duration_since(anchor).num_seconds().max(0);
    let periods = elapsed
        .div_euclid(interval_seconds)
        .checked_add(1)
        .context("recurring schedule tick count overflows")?;
    let next_due_at = anchor
        .checked_add_signed(chrono::Duration::seconds(
            interval_seconds
                .checked_mul(periods)
                .context("recurring schedule due time overflows")?,
        ))
        .context("recurring schedule next due timestamp is out of range")?
        .to_rfc3339_opts(SecondsFormat::Millis, true);
    conn.execute(
        "UPDATE recurring_session_tasks SET active_tool_run_id = NULL,
             next_due_at = ?2, updated_at = ?3 WHERE id = ?1 AND active_tool_run_id = ?4",
        rusqlite::params![schedule_id, next_due_at, finished_at, tool_run_id],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;

    fn database_and_session() -> (Database, String) {
        let database = Database::open_in_memory().unwrap();
        let session = database
            .create_session("recurring session task tests")
            .unwrap();
        (database, session.session_id)
    }

    #[test]
    fn create_lists_safe_schedule_metadata_and_rejects_invalid_inputs() {
        let (database, session_id) = database_and_session();
        let created = database
            .create_recurring_session_task(
                &session_id,
                "  Morning review  ",
                "  Review the work from yesterday  ",
                "  Summarize yesterday's work  ",
                3600,
                "2026-01-01T08:00:00+08:00",
            )
            .unwrap();

        assert!(is_canonical_id(&created.schedule_id, "sched"));
        assert_eq!(created.title, "Morning review");
        assert_eq!(created.body, "Review the work from yesterday");
        assert_eq!(created.interval_seconds, 3600);
        assert_eq!(created.anchor_at, "2026-01-01T00:00:00.000Z");
        assert_eq!(created.next_due_at, created.anchor_at);
        let listed = database.list_recurring_session_tasks(&session_id).unwrap();
        assert_eq!(listed, [created]);

        assert!(
            database
                .create_recurring_session_task(
                    &session_id,
                    "Too frequent",
                    "Body",
                    "Prompt",
                    MIN_RECURRING_SESSION_INTERVAL_SECS - 1,
                    "2026-01-01T00:00:00Z",
                )
                .is_err()
        );
        assert!(
            database
                .create_recurring_session_task(
                    &session_id,
                    "Empty prompt",
                    "Body",
                    "  ",
                    MIN_RECURRING_SESSION_INTERVAL_SECS,
                    "2026-01-01T00:00:00Z",
                )
                .is_err()
        );
    }

    #[test]
    fn due_claim_creates_one_standard_occurrence_and_prevents_overlap() {
        let (database, session_id) = database_and_session();
        let schedule = database
            .create_recurring_session_task(
                &session_id,
                "Daily review",
                "Review the day's work",
                "Review the open work",
                3600,
                "2026-01-01T00:00:00Z",
            )
            .unwrap();

        let claimed = database
            .claim_due_recurring_session_tasks("2026-01-01T00:00:00Z", 32)
            .unwrap();
        assert_eq!(claimed.len(), 1);
        assert_eq!(claimed[0].mode, "new_session");
        assert_eq!(claimed[0].session_id.as_deref(), Some(session_id.as_str()));
        assert_eq!(claimed[0].prompt.as_deref(), Some("Review the open work"));
        assert_eq!(claimed[0].body, "Review the day's work");
        assert!(claimed[0].tool_name.is_none());
        assert!(claimed[0].tool_args.is_none());
        assert!(
            database
                .claim_due_recurring_session_tasks("2026-01-01T00:00:00Z", 32)
                .unwrap()
                .is_empty()
        );

        let listed = database.list_recurring_session_tasks(&session_id).unwrap();
        assert_eq!(listed[0].schedule_id, schedule.schedule_id);
        assert_eq!(
            listed[0].active_tool_run_id.as_deref(),
            Some(claimed[0].tool_run_id.as_str())
        );
    }

    #[test]
    fn terminal_settlement_skips_missed_ticks_and_releases_the_next_occurrence() {
        let (database, session_id) = database_and_session();
        database
            .create_recurring_session_task(
                &session_id,
                "Hourly review",
                "Review the work",
                "Review the current work",
                60,
                "2026-01-01T00:00:00Z",
            )
            .unwrap();
        let claimed = database
            .claim_due_recurring_session_tasks("2026-01-01T00:00:00Z", 32)
            .unwrap();
        let tool_run_id = &claimed[0].tool_run_id;
        assert!(
            database
                .start_scheduled_tool_run(tool_run_id, "2026-01-01T00:00:00Z")
                .unwrap()
        );
        assert!(
            database
                .finish_scheduled_tool_run(
                    tool_run_id,
                    ToolRunStatus::Completed,
                    Some("created session"),
                    None,
                    "2026-01-01T00:03:05Z",
                )
                .unwrap()
        );

        let listed = database.list_recurring_session_tasks(&session_id).unwrap();
        assert_eq!(listed[0].next_due_at, "2026-01-01T00:04:00.000Z");
        assert!(listed[0].active_tool_run_id.is_none());
        assert!(
            database
                .claim_due_recurring_session_tasks("2026-01-01T00:03:59Z", 32)
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            database
                .claim_due_recurring_session_tasks("2026-01-01T00:04:00Z", 32)
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn deleting_a_schedule_returns_its_active_occurrence_for_standard_cancellation() {
        let (database, session_id) = database_and_session();
        let schedule = database
            .create_recurring_session_task(
                &session_id,
                "Review",
                "Review today's work",
                "Review today's changes",
                3600,
                "2026-01-01T00:00:00Z",
            )
            .unwrap();
        let claimed = database
            .claim_due_recurring_session_tasks("2026-01-01T00:00:00Z", 32)
            .unwrap();

        assert_eq!(
            database
                .delete_recurring_session_task(&schedule.schedule_id, &session_id)
                .unwrap(),
            (true, Some(claimed[0].tool_run_id.clone()))
        );
        assert!(
            database
                .list_recurring_session_tasks(&session_id)
                .unwrap()
                .is_empty()
        );
        assert!(
            database
                .get_tool_run(&claimed[0].tool_run_id)
                .unwrap()
                .is_some()
        );
    }
}
