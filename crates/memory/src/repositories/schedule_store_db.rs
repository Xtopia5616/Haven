use anyhow::Context as _;
use chrono::{DateTime, SecondsFormat, Utc};
use haven_common::lifecycle::ScheduleRunStatus;
use haven_common::types::{is_canonical_id, new_id};
use rusqlite::OptionalExtension;

use super::schedule_store_types::*;
use crate::Database;
use crate::repositories::messages::now_rfc3339_millis;
impl Database {
    pub(super) fn create_schedule(
        &self,
        input: SchedulePlanInput,
    ) -> anyhow::Result<SchedulePlanRow> {
        validate_plan_input(&input)?;
        let due_at = input
            .due_at
            .as_deref()
            .map(|value| canonical_timestamp(value, "due_at"))
            .transpose()?;
        let interval_seconds = input
            .interval_seconds
            .map(|value| i64::try_from(value).context("interval_seconds overflows"))
            .transpose()?;
        let title = input.title.trim().to_owned();
        let body = input.body.trim().to_owned();
        let tool_name = input
            .tool_name
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned);
        let prompt = input
            .prompt
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned);
        let tool_args = input.tool_args.map(|value| value.to_string());
        let watch_tool_run_id = input.watch_tool_run_id;
        let schedule_id = new_id("sched");
        let schedule_run_id = (input.schedule_type == ScheduleType::Once).then(|| new_id("schrun"));
        let now = now_rfc3339_millis();
        let schedule_type = input.schedule_type.as_str();
        let mode = input.mode.as_str();
        let conn = self.conn();
        conn.execute_batch("BEGIN IMMEDIATE")?;
        let result = (|| -> anyhow::Result<SchedulePlanRow> {
            let owner_exists: bool = conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM sessions WHERE id = ?1)",
                [&input.owner_session_id],
                |row| row.get(0),
            )?;
            anyhow::ensure!(
                owner_exists,
                "owner session '{}' not found",
                input.owner_session_id
            );
            if let Some(watch_tool_run_id) = watch_tool_run_id.as_deref() {
                let exists: bool = conn.query_row(
                    "SELECT EXISTS(SELECT 1 FROM tool_runs WHERE id = ?1)",
                    [watch_tool_run_id],
                    |row| row.get(0),
                )?;
                anyhow::ensure!(exists, "watched ToolRun '{watch_tool_run_id}' not found");
            }
            let count: i64 = conn.query_row(
                "SELECT COUNT(*) FROM schedules WHERE enabled = 1",
                [],
                |row| row.get(0),
            )?;
            anyhow::ensure!(
                usize::try_from(count).unwrap_or(usize::MAX) < MAX_SCHEDULES,
                "too many active schedules (maximum {MAX_SCHEDULES})"
            );
            let anchor_at = if input.schedule_type == ScheduleType::RecurringSession {
                due_at.clone()
            } else {
                None
            };
            let next_due_at = due_at.clone();
            conn.execute(
                "INSERT INTO schedules
                    (id, owner_session_id, schedule_type, title, body, mode, due_at,
                     watch_tool_run_id, tool_name, tool_args, prompt, interval_seconds,
                     anchor_at, next_due_at, enabled, active_schedule_run_id, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14,
                         1, ?15, ?16, ?16)",
                rusqlite::params![
                    schedule_id,
                    input.owner_session_id,
                    schedule_type,
                    title,
                    body,
                    mode,
                    due_at,
                    watch_tool_run_id,
                    tool_name,
                    tool_args,
                    prompt,
                    interval_seconds,
                    anchor_at,
                    next_due_at,
                    schedule_run_id,
                    now
                ],
            )?;
            if let Some(schedule_run_id) = schedule_run_id.as_deref() {
                conn.execute(
                    "INSERT INTO schedule_runs
                        (id, schedule_id, owner_session_id, due_at, status, created_at)
                     VALUES (?1, ?2, ?3, ?4, 'waiting', ?5)",
                    rusqlite::params![
                        schedule_run_id,
                        schedule_id,
                        input.owner_session_id,
                        due_at,
                        now
                    ],
                )?;
            }
            let sql = format!("SELECT {PLAN_COLUMNS} FROM schedules WHERE id = ?1");
            let row = conn.query_row(&sql, [&schedule_id], plan_from_row)?;
            validate_plan_row(&row)?;
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

    pub(super) fn list_schedules_for_session(
        &self,
        owner_session_id: &str,
    ) -> anyhow::Result<Vec<SchedulePlanRow>> {
        anyhow::ensure!(
            is_canonical_id(owner_session_id, "ses"),
            "invalid owner_session_id"
        );
        let sql = format!(
            "SELECT {PLAN_COLUMNS} FROM schedules
             WHERE owner_session_id = ?1 AND enabled = 1
             ORDER BY COALESCE(next_due_at, due_at), created_at, id"
        );
        let conn = self.conn();
        let mut statement = conn.prepare(&sql)?;
        let rows = statement.query_map([owner_session_id], plan_from_row)?;
        let mut plans = Vec::new();
        for row in rows {
            let row = row?;
            validate_plan_row(&row)?;
            plans.push(row);
        }
        Ok(plans)
    }

    pub(super) fn list_enabled_schedules(&self) -> anyhow::Result<Vec<SchedulePlanRow>> {
        let sql = format!(
            "SELECT {PLAN_COLUMNS} FROM schedules WHERE enabled = 1
             ORDER BY COALESCE(next_due_at, due_at), created_at, id"
        );
        let conn = self.conn();
        let mut statement = conn.prepare(&sql)?;
        let rows = statement.query_map([], plan_from_row)?;
        let mut plans = Vec::new();
        for row in rows {
            let row = row?;
            validate_plan_row(&row)?;
            plans.push(row);
        }
        Ok(plans)
    }

    pub(super) fn list_live_schedule_runs(&self) -> anyhow::Result<Vec<ScheduleRunRow>> {
        let sql = format!(
            "SELECT {RUN_COLUMNS} FROM schedule_runs
             WHERE status IN ('waiting','running') ORDER BY due_at, created_at, id"
        );
        let conn = self.conn();
        let mut statement = conn.prepare(&sql)?;
        let rows = statement.query_map([], run_from_row)?;
        let mut runs = Vec::new();
        for row in rows {
            runs.push(row?);
        }
        Ok(runs)
    }

    pub(super) fn get_schedule_run(
        &self,
        schedule_run_id: &str,
    ) -> anyhow::Result<Option<ScheduleRunRow>> {
        anyhow::ensure!(
            is_canonical_id(schedule_run_id, "schrun"),
            "invalid schedule_run_id"
        );
        let sql = format!("SELECT {RUN_COLUMNS} FROM schedule_runs WHERE id = ?1");
        let conn = self.conn();
        let row = conn
            .query_row(&sql, [schedule_run_id], run_from_row)
            .optional()?;
        if let Some(row) = &row {
            validate_run_row(row)?;
        }
        Ok(row)
    }

    pub(super) fn claim_due_schedule_runs(
        &self,
        now: &str,
        limit: usize,
    ) -> anyhow::Result<Vec<ScheduleRunFired>> {
        let now = canonical_timestamp(now, "now")?;
        if limit == 0 {
            return Ok(Vec::new());
        }
        let limit = i64::try_from(limit.min(128))?;
        let conn = self.conn();
        conn.execute_batch("BEGIN IMMEDIATE")?;
        let result = (|| -> anyhow::Result<Vec<ScheduleRunFired>> {
            let due = {
                let mut statement = conn.prepare(
                    "SELECT s.id, s.schedule_type, s.active_schedule_run_id
                     FROM schedules AS s
                     LEFT JOIN schedule_runs AS active ON active.id = s.active_schedule_run_id
                     LEFT JOIN tool_runs AS watched ON watched.id = s.watch_tool_run_id
                     WHERE s.enabled = 1 AND (
                        (s.schedule_type = 'once' AND active.status = 'waiting' AND (
                            (s.due_at IS NOT NULL AND s.due_at <= ?1) OR
                            (s.watch_tool_run_id IS NOT NULL AND
                                (watched.id IS NULL OR watched.status IN ('completed','failed','cancelled')))
                        )) OR
                        (s.schedule_type = 'recurring_session' AND
                            s.active_schedule_run_id IS NULL AND s.next_due_at <= ?1)
                     )
                     ORDER BY COALESCE(s.next_due_at, s.due_at), s.id LIMIT ?2",
                )?;
                let rows = statement.query_map(rusqlite::params![now, limit], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<String>>(2)?,
                    ))
                })?;
                rows.collect::<Result<Vec<_>, _>>()?
            };
            let mut fired = Vec::with_capacity(due.len());
            for (schedule_id, schedule_type, existing_run_id) in due {
                let schedule_type = ScheduleType::parse(&schedule_type)
                    .ok_or_else(|| anyhow::anyhow!("invalid schedule_type"))?;
                let (schedule_run_id, due_at) = match schedule_type {
                    ScheduleType::Once => {
                        let run_id = existing_run_id
                            .ok_or_else(|| anyhow::anyhow!("one-off schedule has no active run"))?;
                        let due_at: Option<String> = conn.query_row(
                            "SELECT due_at FROM schedule_runs WHERE id = ?1",
                            [&run_id],
                            |row| row.get(0),
                        )?;
                        (run_id, due_at)
                    }
                    ScheduleType::RecurringSession => {
                        let due_at: String = conn.query_row(
                            "SELECT next_due_at FROM schedules WHERE id = ?1",
                            [&schedule_id],
                            |row| row.get(0),
                        )?;
                        let run_id = new_id("schrun");
                        conn.execute(
                            "INSERT INTO schedule_runs
                                (id, schedule_id, owner_session_id, due_at, status, created_at)
                             SELECT ?1, id, owner_session_id, next_due_at, 'waiting', ?2
                             FROM schedules WHERE id = ?3 AND enabled = 1
                                AND active_schedule_run_id IS NULL AND next_due_at <= ?2",
                            rusqlite::params![run_id, now, schedule_id],
                        )?;
                        let changed = conn.execute(
                            "UPDATE schedules SET active_schedule_run_id = ?2, updated_at = ?3
                             WHERE id = ?1 AND enabled = 1 AND active_schedule_run_id IS NULL",
                            rusqlite::params![schedule_id, run_id, now],
                        )?;
                        anyhow::ensure!(changed == 1, "recurring schedule claim was lost");
                        (run_id, Some(due_at))
                    }
                };
                let changed = conn.execute(
                    "UPDATE schedule_runs SET status = 'running', started_at = ?2
                     WHERE id = ?1 AND status = 'waiting'",
                    rusqlite::params![schedule_run_id, now],
                )?;
                if changed == 0 {
                    continue;
                }
                let schedule = {
                    let sql = format!("SELECT {PLAN_COLUMNS} FROM schedules WHERE id = ?1");
                    conn.query_row(&sql, [&schedule_id], plan_from_row)?
                };
                validate_plan_row(&schedule)?;
                let run = {
                    let sql = format!("SELECT {RUN_COLUMNS} FROM schedule_runs WHERE id = ?1");
                    conn.query_row(&sql, [&schedule_run_id], run_from_row)?
                };
                let watched_tool_run =
                    if let Some(tool_run_id) = schedule.watch_tool_run_id.as_deref() {
                        let dependency: Option<(String, Option<String>)> = conn
                        .query_row(
                            "SELECT status, COALESCE(result_summary, error_reason, error, output)
                             FROM tool_runs WHERE id = ?1",
                            [tool_run_id],
                            |row| Ok((row.get(0)?, row.get(1)?)),
                        )
                        .optional()?;
                        Some(match dependency {
                            Some((status, result)) => WatchedToolRunResult {
                                tool_run_id: tool_run_id.to_owned(),
                                status,
                                result,
                            },
                            None => WatchedToolRunResult {
                                tool_run_id: tool_run_id.to_owned(),
                                status: "not_found".into(),
                                result: None,
                            },
                        })
                    } else {
                        None
                    };
                let _ = due_at;
                fired.push(ScheduleRunFired {
                    run,
                    plan: schedule,
                    watched_tool_run,
                });
            }
            Ok(fired)
        })();
        match result {
            Ok(fired) => {
                conn.execute_batch("COMMIT")?;
                Ok(fired)
            }
            Err(error) => {
                let _ = conn.execute_batch("ROLLBACK");
                Err(error)
            }
        }
    }

    pub(super) fn cancel_schedule(
        &self,
        schedule_id: &str,
        owner_session_id: &str,
        finished_at: &str,
    ) -> anyhow::Result<bool> {
        anyhow::ensure!(is_canonical_id(schedule_id, "sched"), "invalid schedule_id");
        anyhow::ensure!(
            is_canonical_id(owner_session_id, "ses"),
            "invalid owner_session_id"
        );
        let finished_at = canonical_timestamp(finished_at, "finished_at")?;
        let conn = self.conn();
        conn.execute_batch("BEGIN IMMEDIATE")?;
        let result = (|| -> anyhow::Result<bool> {
            let plan: Option<(Option<String>, bool)> = conn
                .query_row(
                    "SELECT active_schedule_run_id, enabled FROM schedules
                     WHERE id = ?1 AND owner_session_id = ?2",
                    rusqlite::params![schedule_id, owner_session_id],
                    |row| Ok((row.get(0)?, row.get::<_, i64>(1)? == 1)),
                )
                .optional()?;
            let Some((active_run_id, enabled)) = plan else {
                return Ok(false);
            };
            if !enabled {
                return Ok(false);
            }
            if let Some(run_id) = active_run_id.as_deref() {
                let claimed: bool = conn.query_row(
                    "SELECT EXISTS(SELECT 1 FROM schedule_execution_claims WHERE schedule_run_id = ?1)",
                    [run_id],
                    |row| row.get(0),
                )?;
                if claimed {
                    return Ok(false);
                }
                conn.execute(
                    "UPDATE schedule_runs SET status = 'cancelled', finished_at = ?2
                     WHERE id = ?1 AND status IN ('waiting','running')",
                    rusqlite::params![run_id, finished_at],
                )?;
            }
            let changed = conn.execute(
                "UPDATE schedules SET enabled = 0, active_schedule_run_id = NULL, updated_at = ?3
                 WHERE id = ?1 AND owner_session_id = ?2 AND enabled = 1",
                rusqlite::params![schedule_id, owner_session_id, finished_at],
            )?;
            Ok(changed == 1)
        })();
        match result {
            Ok(cancelled) => {
                conn.execute_batch("COMMIT")?;
                Ok(cancelled)
            }
            Err(error) => {
                let _ = conn.execute_batch("ROLLBACK");
                Err(error)
            }
        }
    }

    pub(super) fn claim_schedule_run_execution(
        &self,
        schedule_run_id: &str,
        claim_id: &str,
    ) -> anyhow::Result<bool> {
        anyhow::ensure!(
            is_canonical_id(schedule_run_id, "schrun"),
            "invalid schedule_run_id"
        );
        anyhow::ensure!(
            is_canonical_id(claim_id, "conf")
                || (claim_id == schedule_run_id && is_canonical_id(claim_id, "schrun")),
            "invalid claim_id: expected canonical conf- ID or the claimed schrun- ID"
        );
        let conn = self.conn();
        conn.execute_batch("BEGIN IMMEDIATE")?;
        let result = (|| -> anyhow::Result<bool> {
            let running: bool = conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM schedule_runs WHERE id = ?1 AND status = 'running')",
                [schedule_run_id],
                |row| row.get(0),
            )?;
            if !running {
                return Ok(false);
            }
            conn.execute(
                "INSERT OR IGNORE INTO schedule_execution_claims(schedule_run_id, claim_id)
                 VALUES (?1, ?2)",
                rusqlite::params![schedule_run_id, claim_id],
            )?;
            let durable_claim: String = conn.query_row(
                "SELECT claim_id FROM schedule_execution_claims WHERE schedule_run_id = ?1",
                [schedule_run_id],
                |row| row.get(0),
            )?;
            Ok(durable_claim == claim_id)
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

    pub(super) fn release_schedule_run_execution(
        &self,
        schedule_run_id: &str,
        claim_id: &str,
    ) -> anyhow::Result<bool> {
        anyhow::ensure!(
            is_canonical_id(schedule_run_id, "schrun"),
            "invalid schedule_run_id"
        );
        anyhow::ensure!(
            is_canonical_id(claim_id, "conf")
                || (claim_id == schedule_run_id && is_canonical_id(claim_id, "schrun")),
            "invalid claim_id"
        );
        Ok(self.conn().execute(
            "DELETE FROM schedule_execution_claims WHERE schedule_run_id = ?1 AND claim_id = ?2
             AND EXISTS(SELECT 1 FROM schedule_runs WHERE id = ?1 AND status = 'running')",
            rusqlite::params![schedule_run_id, claim_id],
        )? > 0)
    }

    pub(super) fn finish_schedule_run(
        &self,
        schedule_run_id: &str,
        status: ScheduleRunStatus,
        result_summary: Option<&str>,
        error_reason: Option<&str>,
        finished_at: &str,
    ) -> anyhow::Result<bool> {
        anyhow::ensure!(
            is_canonical_id(schedule_run_id, "schrun"),
            "invalid schedule_run_id"
        );
        anyhow::ensure!(
            status.is_terminal(),
            "ScheduleRun terminal status is required"
        );
        let finished_at = canonical_timestamp(finished_at, "finished_at")?;
        let conn = self.conn();
        conn.execute_batch("BEGIN IMMEDIATE")?;
        let result = (|| -> anyhow::Result<bool> {
            let meta: Option<(String, String)> = conn
                .query_row(
                    "SELECT schedule_id, status FROM schedule_runs WHERE id = ?1",
                    [schedule_run_id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()?;
            let Some((schedule_id, old_status)) = meta else {
                return Ok(false);
            };
            if old_status != "running" {
                return Ok(false);
            }
            let Some((schedule_type, anchor_at, interval_seconds)) = conn
                .query_row(
                    "SELECT schedule_type, anchor_at, interval_seconds FROM schedules WHERE id = ?1",
                    [&schedule_id],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?, row.get::<_, Option<i64>>(2)?)),
                )
                .optional()?
            else {
                anyhow::bail!("Schedule plan '{schedule_id}' not found");
            };
            conn.execute(
                "UPDATE schedule_runs SET status = ?2, result_summary = ?3, error_reason = ?4,
                     finished_at = ?5 WHERE id = ?1 AND status = 'running'",
                rusqlite::params![
                    schedule_run_id,
                    status.as_str(),
                    result_summary,
                    error_reason,
                    finished_at
                ],
            )?;
            conn.execute(
                "DELETE FROM schedule_execution_claims WHERE schedule_run_id = ?1",
                [schedule_run_id],
            )?;
            if schedule_type == "once" {
                conn.execute(
                    "UPDATE schedules SET enabled = 0, active_schedule_run_id = NULL, updated_at = ?2
                     WHERE id = ?1 AND active_schedule_run_id = ?3",
                    rusqlite::params![schedule_id, finished_at, schedule_run_id],
                )?;
            } else {
                let anchor_at =
                    anchor_at.ok_or_else(|| anyhow::anyhow!("missing schedule anchor"))?;
                let interval_seconds = u64::try_from(
                    interval_seconds.ok_or_else(|| anyhow::anyhow!("missing schedule interval"))?,
                )?;
                let anchor = DateTime::parse_from_rfc3339(&anchor_at)?.with_timezone(&Utc);
                let finished = DateTime::parse_from_rfc3339(&finished_at)?.with_timezone(&Utc);
                let interval = i64::try_from(interval_seconds)?;
                let periods = finished
                    .signed_duration_since(anchor)
                    .num_seconds()
                    .max(0)
                    .div_euclid(interval)
                    .checked_add(1)
                    .context("recurring Schedule tick count overflows")?;
                let next_due_at = anchor
                    .checked_add_signed(chrono::Duration::seconds(
                        interval
                            .checked_mul(periods)
                            .context("recurring Schedule due time overflows")?,
                    ))
                    .context("recurring Schedule next due timestamp is out of range")?
                    .to_rfc3339_opts(SecondsFormat::Millis, true);
                conn.execute(
                    "UPDATE schedules SET active_schedule_run_id = NULL, next_due_at = ?2, updated_at = ?3
                     WHERE id = ?1 AND active_schedule_run_id = ?4 AND enabled = 1",
                    rusqlite::params![schedule_id, next_due_at, finished_at, schedule_run_id],
                )?;
            }
            Ok(true)
        })();
        match result {
            Ok(finished) => {
                conn.execute_batch("COMMIT")?;
                Ok(finished)
            }
            Err(error) => {
                let _ = conn.execute_batch("ROLLBACK");
                Err(error)
            }
        }
    }

    pub(super) fn fail_interrupted_schedule_runs(
        &self,
        finished_at: &str,
    ) -> anyhow::Result<usize> {
        let finished_at = canonical_timestamp(finished_at, "finished_at")?;
        let rows = self.list_live_schedule_runs()?;
        let mut failed = 0;
        for row in rows
            .into_iter()
            .filter(|row| row.status == ScheduleRunStatus::Running)
        {
            failed += usize::from(self.finish_schedule_run(
                &row.schedule_run_id,
                ScheduleRunStatus::Failed,
                None,
                Some("interrupted by process restart"),
                &finished_at,
            )?);
        }
        Ok(failed)
    }
}
