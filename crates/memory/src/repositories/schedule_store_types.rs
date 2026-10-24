//! Typed asynchronous persistence boundary for Schedule plans and occurrences.
//!
//! Schedule owns trigger plans and durable occurrences. It does not persist
//! trigger rows in ToolRun storage; a ScheduleRun only becomes a ToolCall (or
//! a session input) after the trigger has been claimed.

use anyhow::Context as _;
use chrono::{DateTime, SecondsFormat, Utc};
use haven_common::lifecycle::ScheduleRunStatus;
use haven_common::schedule::ScheduleMode;
use haven_common::types::is_canonical_id;

pub(super) const MAX_SCHEDULES: usize = 256;
const MAX_SCHEDULE_DUE_HORIZON_SECS: i64 = 10 * 365 * 24 * 60 * 60;
const MAX_SCHEDULE_TITLE_CHARS: usize = 80;
const MAX_SCHEDULE_BODY_CHARS: usize = 2_000;
const MAX_SCHEDULE_PROMPT_BYTES: usize = 65_536;
const MIN_RECURRING_INTERVAL_SECS: u64 = 60;
const MAX_RECURRING_INTERVAL_SECS: u64 = 31_536_000;

/// Type of durable Schedule plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScheduleType {
    Once,
    RecurringSession,
}

impl ScheduleType {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::Once => "once",
            Self::RecurringSession => "recurring_session",
        }
    }

    pub(super) fn parse(value: &str) -> Option<Self> {
        match value {
            "once" => Some(Self::Once),
            "recurring_session" => Some(Self::RecurringSession),
            _ => None,
        }
    }
}

/// A complete plan to create. Exactly one of `due_at` and
/// `watch_tool_run_id` is required for a one-off schedule; recurring schedules
/// require a time trigger and `ScheduleMode::NewSession`.
#[derive(Debug, Clone)]
pub struct SchedulePlanInput {
    pub owner_session_id: String,
    pub schedule_type: ScheduleType,
    pub title: String,
    pub body: String,
    pub mode: ScheduleMode,
    pub due_at: Option<String>,
    pub watch_tool_run_id: Option<String>,
    pub tool_name: Option<String>,
    pub tool_args: Option<serde_json::Value>,
    pub prompt: Option<String>,
    pub interval_seconds: Option<u64>,
}

/// Durable Schedule plan projection. `prompt` is intentionally included for
/// the internal runtime but callers must omit it from user-visible list views.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SchedulePlanRow {
    pub schedule_id: String,
    pub owner_session_id: String,
    pub schedule_type: ScheduleType,
    pub title: String,
    pub body: String,
    pub mode: ScheduleMode,
    pub due_at: Option<String>,
    pub watch_tool_run_id: Option<String>,
    pub tool_name: Option<String>,
    pub tool_args: Option<serde_json::Value>,
    pub prompt: Option<String>,
    pub interval_seconds: Option<u64>,
    pub anchor_at: Option<String>,
    pub next_due_at: Option<String>,
    pub enabled: bool,
    pub active_schedule_run_id: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// Durable state of one Schedule occurrence.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ScheduleRunRow {
    pub schedule_run_id: String,
    pub schedule_id: String,
    pub owner_session_id: String,
    pub due_at: Option<String>,
    pub status: ScheduleRunStatus,
    pub result_summary: Option<String>,
    pub error_reason: Option<String>,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
    pub created_at: String,
}

/// Outcomes from recovering ScheduleRun rows left `running` by a prior process.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ScheduleRecoverySummary {
    /// Rows with no durable execution claim were never admitted to side-effect
    /// execution and can be retried with the same occurrence identity.
    pub requeued_before_execution: usize,
    /// Rows with an execution claim may have started a side effect; they are
    /// terminally failed with an explicit unknown-outcome reason.
    pub failed_with_unknown_outcome: usize,
}

/// Producer result supplied when a one-off schedule waits for a ToolRun.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WatchedToolRunResult {
    pub tool_run_id: String,
    pub status: String,
    pub result: Option<String>,
}

/// Schedule payload atomically claimed for execution.
#[derive(Debug, Clone, PartialEq)]
pub struct ScheduleRunFired {
    pub run: ScheduleRunRow,
    pub plan: SchedulePlanRow,
    pub watched_tool_run: Option<WatchedToolRunResult>,
}

pub(super) fn canonical_timestamp(value: &str, field: &str) -> anyhow::Result<String> {
    let timestamp = DateTime::parse_from_rfc3339(value)
        .with_context(|| format!("{field} must be an RFC3339 timestamp"))?;
    Ok(timestamp
        .with_timezone(&Utc)
        .to_rfc3339_opts(SecondsFormat::Millis, true))
}

fn canonical_persisted_timestamp(value: &str, field: &str) -> anyhow::Result<String> {
    let canonical = canonical_timestamp(value, field)?;
    anyhow::ensure!(value == canonical, "invalid persisted {field}");
    Ok(canonical)
}

pub(super) fn validate_plan_input(input: &SchedulePlanInput) -> anyhow::Result<()> {
    anyhow::ensure!(
        is_canonical_id(&input.owner_session_id, "ses"),
        "invalid owner_session_id"
    );
    anyhow::ensure!(
        !input.title.trim().is_empty()
            && input.title.trim().chars().count() <= MAX_SCHEDULE_TITLE_CHARS,
        "schedule title must contain 1 to {MAX_SCHEDULE_TITLE_CHARS} characters"
    );
    anyhow::ensure!(
        !input.body.trim().is_empty()
            && input.body.trim().chars().count() <= MAX_SCHEDULE_BODY_CHARS,
        "schedule body must contain 1 to {MAX_SCHEDULE_BODY_CHARS} characters"
    );
    anyhow::ensure!(
        input.due_at.is_some() != input.watch_tool_run_id.is_some(),
        "schedule requires exactly one of due_at or watch_tool_run_id"
    );
    if let Some(due_at) = &input.due_at {
        let due_at = DateTime::parse_from_rfc3339(due_at)
            .with_context(|| "due_at must be an RFC3339 timestamp")?
            .with_timezone(&Utc);
        let horizon = due_at.signed_duration_since(Utc::now()).num_seconds();
        anyhow::ensure!(
            horizon <= MAX_SCHEDULE_DUE_HORIZON_SECS,
            "schedule due_at cannot be more than 10 years in the future"
        );
    }
    if let Some(tool_run_id) = &input.watch_tool_run_id {
        anyhow::ensure!(
            is_canonical_id(tool_run_id, "toolrun"),
            "invalid watch_tool_run_id"
        );
    }
    let tool_name = input
        .tool_name
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty());
    let prompt = input
        .prompt
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    anyhow::ensure!(
        (input.mode == ScheduleMode::Tool) == tool_name.is_some(),
        "tool_name is required only when mode is 'tool'"
    );
    anyhow::ensure!(
        input.mode == ScheduleMode::Tool || input.tool_args.is_none(),
        "tool_args require mode 'tool'"
    );
    if let Some(args) = &input.tool_args {
        anyhow::ensure!(args.is_object(), "tool_args must be a JSON object");
    }
    anyhow::ensure!(
        (input.mode == ScheduleMode::Tool) == prompt.is_none(),
        "prompt is required only for continue and new_session modes"
    );
    if let Some(prompt) = prompt {
        anyhow::ensure!(
            prompt.len() <= MAX_SCHEDULE_PROMPT_BYTES,
            "schedule prompt exceeds the 64 KiB limit"
        );
    }
    match input.schedule_type {
        ScheduleType::Once => anyhow::ensure!(
            input.interval_seconds.is_none(),
            "one-off schedule cannot have interval_seconds"
        ),
        ScheduleType::RecurringSession => {
            let interval = input
                .interval_seconds
                .ok_or_else(|| anyhow::anyhow!("recurring schedule requires interval_seconds"))?;
            anyhow::ensure!(
                (MIN_RECURRING_INTERVAL_SECS..=MAX_RECURRING_INTERVAL_SECS).contains(&interval),
                "recurring interval must be between 60 seconds and 1 year"
            );
            anyhow::ensure!(
                input.due_at.is_some()
                    && input.watch_tool_run_id.is_none()
                    && input.mode == ScheduleMode::NewSession,
                "recurring schedules require a timed new_session mode"
            );
        }
    }
    Ok(())
}

pub(super) fn plan_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<SchedulePlanRow> {
    let schedule_id: String = row.get(0)?;
    let owner_session_id: String = row.get(1)?;
    let schedule_type: String = row.get(2)?;
    let title: String = row.get(3)?;
    let body: String = row.get(4)?;
    let mode: String = row.get(5)?;
    let due_at: Option<String> = row.get(6)?;
    let watch_tool_run_id: Option<String> = row.get(7)?;
    let tool_name: Option<String> = row.get(8)?;
    let tool_args: Option<String> = row.get(9)?;
    let prompt: Option<String> = row.get(10)?;
    let interval_seconds: Option<i64> = row.get(11)?;
    let anchor_at: Option<String> = row.get(12)?;
    let next_due_at: Option<String> = row.get(13)?;
    let enabled: bool = row.get::<_, i64>(14)? == 1;
    let active_schedule_run_id: Option<String> = row.get(15)?;
    let created_at: String = row.get(16)?;
    let updated_at: String = row.get(17)?;
    let parse_error = || {
        rusqlite::Error::FromSqlConversionFailure(
            0,
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "invalid persisted Schedule plan",
            )),
        )
    };
    let Some(schedule_type) = ScheduleType::parse(&schedule_type) else {
        return Err(parse_error());
    };
    let Some(mode) = ScheduleMode::parse(&mode) else {
        return Err(parse_error());
    };
    let tool_args = tool_args
        .map(|value| serde_json::from_str(&value).map_err(|_| parse_error()))
        .transpose()?;
    Ok(SchedulePlanRow {
        schedule_id,
        owner_session_id,
        schedule_type,
        title,
        body,
        mode,
        due_at,
        watch_tool_run_id,
        tool_name,
        tool_args,
        prompt,
        interval_seconds: interval_seconds.and_then(|value| u64::try_from(value).ok()),
        anchor_at,
        next_due_at,
        enabled,
        active_schedule_run_id,
        created_at,
        updated_at,
    })
}

pub(super) const PLAN_COLUMNS: &str =
    "id, owner_session_id, schedule_type, title, body, mode, due_at,
    watch_tool_run_id, tool_name, tool_args, prompt, interval_seconds, anchor_at,
    next_due_at, enabled, active_schedule_run_id, created_at, updated_at";

pub(super) fn validate_plan_row(row: &SchedulePlanRow) -> anyhow::Result<()> {
    anyhow::ensure!(
        is_canonical_id(&row.schedule_id, "sched"),
        "invalid schedule_id"
    );
    anyhow::ensure!(
        is_canonical_id(&row.owner_session_id, "ses"),
        "invalid owner_session_id"
    );
    if let Some(run_id) = &row.active_schedule_run_id {
        anyhow::ensure!(
            is_canonical_id(run_id, "schrun"),
            "invalid active_schedule_run_id"
        );
    }
    if let Some(tool_run_id) = &row.watch_tool_run_id {
        anyhow::ensure!(
            is_canonical_id(tool_run_id, "toolrun"),
            "invalid watch_tool_run_id"
        );
    }
    for (value, field) in [
        (row.due_at.as_deref(), "due_at"),
        (row.anchor_at.as_deref(), "anchor_at"),
        (row.next_due_at.as_deref(), "next_due_at"),
        (Some(row.created_at.as_str()), "created_at"),
        (Some(row.updated_at.as_str()), "updated_at"),
    ] {
        if let Some(value) = value {
            canonical_persisted_timestamp(value, field)?;
        }
    }
    Ok(())
}

pub(super) fn run_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ScheduleRunRow> {
    let status: String = row.get(4)?;
    let status = match status.as_str() {
        "waiting" => ScheduleRunStatus::Waiting,
        "running" => ScheduleRunStatus::Running,
        "completed" => ScheduleRunStatus::Completed,
        "failed" => ScheduleRunStatus::Failed,
        "cancelled" => ScheduleRunStatus::Cancelled,
        _ => {
            return Err(rusqlite::Error::FromSqlConversionFailure(
                4,
                rusqlite::types::Type::Text,
                Box::new(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "invalid persisted ScheduleRun status",
                )),
            ));
        }
    };
    Ok(ScheduleRunRow {
        schedule_run_id: row.get(0)?,
        schedule_id: row.get(1)?,
        owner_session_id: row.get(2)?,
        due_at: row.get(3)?,
        status,
        result_summary: row.get(5)?,
        error_reason: row.get(6)?,
        started_at: row.get(7)?,
        finished_at: row.get(8)?,
        created_at: row.get(9)?,
    })
}

pub(super) const RUN_COLUMNS: &str = "id, schedule_id, owner_session_id, due_at, status,
    result_summary, error_reason, started_at, finished_at, created_at";

pub(super) fn validate_run_row(row: &ScheduleRunRow) -> anyhow::Result<()> {
    anyhow::ensure!(
        is_canonical_id(&row.schedule_run_id, "schrun"),
        "invalid schedule_run_id"
    );
    anyhow::ensure!(
        is_canonical_id(&row.schedule_id, "sched"),
        "invalid schedule_id"
    );
    anyhow::ensure!(
        is_canonical_id(&row.owner_session_id, "ses"),
        "invalid owner_session_id"
    );
    for (value, field) in [
        (row.due_at.as_deref(), "due_at"),
        (row.started_at.as_deref(), "started_at"),
        (row.finished_at.as_deref(), "finished_at"),
        (Some(row.created_at.as_str()), "created_at"),
    ] {
        if let Some(value) = value {
            canonical_persisted_timestamp(value, field)?;
        }
    }
    Ok(())
}
