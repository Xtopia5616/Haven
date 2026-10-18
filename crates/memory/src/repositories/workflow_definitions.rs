//! Typed persistence for reusable, versioned Workflow definitions.

use std::sync::Arc;

use crate::Database;
use crate::repositories::tool_runs::ScheduledToolRunRow;
use anyhow::Context;
use haven_common::lifecycle::ToolRunStatus;
use haven_common::types::{is_canonical_id, new_id};
use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};

const MAX_DEFINITION_BYTES: usize = 64 * 1024;
const MAX_NAME_CHARS: usize = 80;
const MAX_DESCRIPTION_CHARS: usize = 1000;
const MIN_SCHEDULE_INTERVAL_SECS: u64 = 60;
const MAX_SCHEDULE_INTERVAL_SECS: u64 = 365 * 24 * 60 * 60;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowDefinitionRow {
    pub workflow_id: String,
    pub name: String,
    pub description: String,
    pub revision: u32,
    pub definition_json: String,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowScheduleRow {
    pub schedule_id: String,
    pub name: String,
    pub workflow_definition_id: Option<String>,
    pub workflow_revision: u32,
    pub definition_json: String,
    pub session_id: String,
    pub interval_seconds: u64,
    pub anchor_at: String,
    pub next_due_at: String,
    pub enabled: bool,
    pub active_tool_run_id: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateWorkflowSchedule {
    pub name: String,
    pub workflow_definition_id: Option<String>,
    pub workflow_revision: u32,
    pub definition_json: String,
    pub session_id: String,
    pub interval_seconds: u64,
    pub anchor_at: String,
}

#[derive(Clone)]
pub struct WorkflowDefinitionStore {
    db: Arc<Database>,
}

impl WorkflowDefinitionStore {
    pub(crate) fn new(db: Arc<Database>) -> Self {
        Self { db }
    }

    pub async fn list(&self) -> anyhow::Result<Vec<WorkflowDefinitionRow>> {
        self.db
            .run_blocking(|db| {
                let conn = db.conn();
                let mut statement = conn.prepare(
                "SELECT id, name, description, revision, definition_json, created_at, updated_at
                 FROM workflow_definitions ORDER BY name COLLATE NOCASE, id",
            )?;
                let rows = statement.query_map([], row_from_sql)?;
                rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
            })
            .await
    }

    pub async fn get(&self, workflow_id: String) -> anyhow::Result<Option<WorkflowDefinitionRow>> {
        validate_id(&workflow_id)?;
        self.db
            .run_blocking(move |db| {
                let conn = db.conn();
                conn.query_row(
                "SELECT id, name, description, revision, definition_json, created_at, updated_at
                 FROM workflow_definitions WHERE id = ?1",
                [&workflow_id],
                row_from_sql,
            )
            .optional()
            .map_err(Into::into)
            })
            .await
    }

    pub async fn create(
        &self,
        name: String,
        description: String,
        definition_json: String,
    ) -> anyhow::Result<WorkflowDefinitionRow> {
        let name = name.trim().to_owned();
        validate_fields(&name, &description, &definition_json)?;
        let workflow_id = new_id("wfdef");
        self.db
            .run_blocking(move |db| {
                let now = now_timestamp();
                let conn = db.conn();
                conn.execute(
                    "INSERT INTO workflow_definitions
                    (id, name, description, revision, definition_json, created_at, updated_at)
                 VALUES (?1, ?2, ?3, 1, ?4, ?5, ?5)",
                    rusqlite::params![workflow_id, name, description, definition_json, now],
                )
                .context("failed to save Workflow definition")?;
                Ok(WorkflowDefinitionRow {
                    workflow_id,
                    name,
                    description,
                    revision: 1,
                    definition_json,
                    created_at: now.clone(),
                    updated_at: now,
                })
            })
            .await
    }

    pub async fn update(
        &self,
        workflow_id: String,
        expected_revision: u32,
        name: String,
        description: String,
        definition_json: String,
    ) -> anyhow::Result<Option<WorkflowDefinitionRow>> {
        validate_id(&workflow_id)?;
        let name = name.trim().to_owned();
        anyhow::ensure!(expected_revision > 0, "expected_revision must be positive");
        validate_fields(&name, &description, &definition_json)?;
        self.db
            .run_blocking(move |db| {
                let now = now_timestamp();
                let conn = db.conn();
                conn.execute_batch("BEGIN IMMEDIATE")?;
                let result = (|| -> anyhow::Result<Option<WorkflowDefinitionRow>> {
                    let changed = conn.execute(
                        "UPDATE workflow_definitions
                     SET name = ?2, description = ?3, definition_json = ?4,
                         revision = revision + 1, updated_at = ?5
                     WHERE id = ?1 AND revision = ?6",
                        rusqlite::params![
                            workflow_id,
                            name,
                            description,
                            definition_json,
                            now,
                            i64::from(expected_revision)
                        ],
                    )?;
                    if changed == 0 {
                        return Ok(None);
                    }
                    let row = conn.query_row(
                    "SELECT id, name, description, revision, definition_json, created_at, updated_at
                     FROM workflow_definitions WHERE id = ?1",
                    [&workflow_id],
                    row_from_sql,
                )?;
                    Ok(Some(row))
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
            })
            .await
    }

    pub async fn delete(&self, workflow_id: String) -> anyhow::Result<bool> {
        validate_id(&workflow_id)?;
        self.db
            .run_blocking(move |db| {
                let conn = db.conn();
                Ok(conn.execute(
                    "DELETE FROM workflow_definitions WHERE id = ?1",
                    [&workflow_id],
                )? > 0)
            })
            .await
    }

    pub async fn list_schedules(&self) -> anyhow::Result<Vec<WorkflowScheduleRow>> {
        self.db
            .run_blocking(|db| {
                let conn = db.conn();
                let mut statement = conn.prepare(
                    "SELECT id, name, workflow_definition_id, workflow_revision, definition_json,
                        session_id, interval_seconds, anchor_at, next_due_at, enabled,
                        active_tool_run_id, created_at, updated_at
                 FROM workflow_schedules ORDER BY enabled DESC, next_due_at, name COLLATE NOCASE",
                )?;
                let rows = statement.query_map([], schedule_row_from_sql)?;
                rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
            })
            .await
    }

    pub async fn create_schedule(
        &self,
        input: CreateWorkflowSchedule,
    ) -> anyhow::Result<WorkflowScheduleRow> {
        let CreateWorkflowSchedule {
            name,
            workflow_definition_id,
            workflow_revision,
            definition_json,
            session_id,
            interval_seconds,
            anchor_at,
        } = input;
        let name = name.trim().to_owned();
        validate_schedule_fields(
            &name,
            workflow_definition_id.as_deref(),
            workflow_revision,
            &definition_json,
            &session_id,
            interval_seconds,
            &anchor_at,
        )?;
        let schedule_id = new_id("wfsched");
        self.db
            .run_blocking(move |db| {
                let now = now_timestamp();
                let conn = db.conn();
                conn.execute(
                    "INSERT INTO workflow_schedules
                    (id, name, workflow_definition_id, workflow_revision, definition_json,
                     session_id, interval_seconds, anchor_at, next_due_at, enabled,
                     active_tool_run_id, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8, 1, NULL, ?9, ?9)",
                    rusqlite::params![
                        schedule_id,
                        name,
                        workflow_definition_id,
                        i64::from(workflow_revision),
                        definition_json,
                        session_id,
                        i64::try_from(interval_seconds).unwrap_or(i64::MAX),
                        anchor_at,
                        now
                    ],
                )
                .context("failed to save recurring Workflow schedule")?;
                Ok(WorkflowScheduleRow {
                    schedule_id,
                    name,
                    workflow_definition_id,
                    workflow_revision,
                    definition_json,
                    session_id,
                    interval_seconds,
                    anchor_at: anchor_at.clone(),
                    next_due_at: anchor_at,
                    enabled: true,
                    active_tool_run_id: None,
                    created_at: now.clone(),
                    updated_at: now,
                })
            })
            .await
    }

    pub async fn set_schedule_enabled(
        &self,
        schedule_id: String,
        enabled: bool,
    ) -> anyhow::Result<bool> {
        validate_schedule_id(&schedule_id)?;
        self.db
            .run_blocking(move |db| {
                let conn = db.conn();
                Ok(conn.execute(
                    "UPDATE workflow_schedules SET enabled = ?2, updated_at = ?3 WHERE id = ?1",
                    rusqlite::params![
                        schedule_id,
                        if enabled { 1i64 } else { 0i64 },
                        now_timestamp()
                    ],
                )? > 0)
            })
            .await
    }

    /// Delete future occurrences. The current ToolRun keeps its own snapshot
    /// and continues independently when the schedule row is removed.
    pub async fn delete_schedule(&self, schedule_id: String) -> anyhow::Result<bool> {
        validate_schedule_id(&schedule_id)?;
        self.db
            .run_blocking(move |db| {
                let conn = db.conn();
                Ok(conn.execute(
                    "DELETE FROM workflow_schedules WHERE id = ?1",
                    [&schedule_id],
                )? > 0)
            })
            .await
    }

    /// Atomically materialize due recurring occurrences and advance each
    /// schedule past every missed tick. The returned waiting rows are restored
    /// through ToolRunService's normal timer, confirmation, and outbox paths.
    pub async fn claim_due_occurrences(
        &self,
        now: String,
    ) -> anyhow::Result<Vec<ScheduledToolRunRow>> {
        let now = normalize_timestamp(&now, "now")?;
        self.db.run_blocking(move |db| {
            let conn = db.conn();
            conn.execute_batch("BEGIN IMMEDIATE")?;
            let result = (|| -> anyhow::Result<Vec<ScheduledToolRunRow>> {
                let due_schedules = {
                    let mut statement = conn.prepare(
                        "SELECT id, name, workflow_definition_id, workflow_revision,
                                definition_json, session_id, interval_seconds, anchor_at, next_due_at
                         FROM workflow_schedules
                         WHERE enabled = 1 AND active_tool_run_id IS NULL AND next_due_at <= ?1
                         ORDER BY next_due_at, id LIMIT 32",
                    )?;
                    let rows = statement.query_map([&now], |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, Option<String>>(2)?,
                            row.get::<_, i64>(3)?,
                            row.get::<_, String>(4)?,
                            row.get::<_, String>(5)?,
                            row.get::<_, i64>(6)?,
                            row.get::<_, String>(7)?,
                            row.get::<_, String>(8)?,
                        ))
                    })?;
                    rows.collect::<Result<Vec<_>, _>>()?
                };

                let mut occurrences = Vec::with_capacity(due_schedules.len());
                for (
                    schedule_id,
                    name,
                    workflow_definition_id,
                    revision,
                    definition_json,
                    session_id,
                    interval_seconds,
                    anchor_at,
                    due_at,
                ) in due_schedules
                {
                    validate_schedule_id(&schedule_id)?;
                    anyhow::ensure!(is_canonical_id(&session_id, "ses"), "invalid schedule session_id");
                    if let Some(definition_id) = workflow_definition_id.as_deref() {
                        anyhow::ensure!(is_canonical_id(definition_id, "wfdef"), "invalid schedule workflow_definition_id");
                    }
                    let revision = u32::try_from(revision).context("invalid schedule workflow_revision")?;
                    let interval_seconds = u64::try_from(interval_seconds).context("invalid schedule interval")?;
                    anyhow::ensure!(
                        (MIN_SCHEDULE_INTERVAL_SECS..=MAX_SCHEDULE_INTERVAL_SECS).contains(&interval_seconds),
                        "invalid schedule interval"
                    );
                    let definition: serde_json::Value = serde_json::from_str(&definition_json)
                        .context("stored Workflow schedule definition is invalid")?;
                    validate_fields(&name, "", &definition_json)?;
                    let due = parse_timestamp(&due_at, "next_due_at")?;
                    let anchor = parse_timestamp(&anchor_at, "anchor_at")?;
                    let now_value = parse_timestamp(&now, "now")?;
                    anyhow::ensure!(anchor <= due && due <= now_value, "schedule timing is invalid");
                    let interval_seconds =
                        i64::try_from(interval_seconds).context("schedule interval overflows")?;
                    let elapsed = now_value.signed_duration_since(anchor).num_seconds();
                    let steps = elapsed
                        .div_euclid(interval_seconds)
                        .checked_add(1)
                        .context("schedule tick count overflows")?;
                    let offset_seconds = interval_seconds
                        .checked_mul(steps)
                        .context("schedule next due time overflows")?;
                    let next_due = anchor
                        .checked_add_signed(chrono::Duration::seconds(offset_seconds))
                        .context("schedule next due time is out of range")?;
                    anyhow::ensure!(next_due > now_value, "schedule did not advance past now");
                    let next_due_at = next_due
                        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
                    let tool_run_id = new_id("toolrun");
                    let args = serde_json::json!({
                        "execution_mode": "inline",
                        "workflow": definition,
                    }).to_string();
                    conn.execute(
                        "INSERT INTO tool_runs
                            (id, kind, due_at, title, body, mode, session_id, tool_name, tool_args,
                             workflow_definition_id, workflow_revision, workflow_schedule_id,
                             status, created_at)
                         VALUES (?1, 'scheduled', ?2, ?3, ?4, 'tool', ?5, 'workflow_run', ?6,
                                 ?7, ?8, ?9, 'waiting', ?10)",
                        rusqlite::params![
                            tool_run_id,
                            due_at,
                            name,
                            format!("Recurring Workflow: {name}"),
                            session_id,
                            args,
                            workflow_definition_id,
                            i64::from(revision),
                            schedule_id,
                            now,
                        ],
                    )?;
                    let changed = conn.execute(
                        "UPDATE workflow_schedules SET active_tool_run_id = ?2,
                             next_due_at = ?3, updated_at = ?4
                         WHERE id = ?1 AND enabled = 1 AND active_tool_run_id IS NULL",
                        rusqlite::params![schedule_id, tool_run_id, next_due_at, now],
                    )?;
                    anyhow::ensure!(changed == 1, "recurring schedule claim was lost");
                    occurrences.push(ScheduledToolRunRow {
                        tool_run_id,
                        due_at,
                        title: name.clone(),
                        body: format!("Recurring Workflow: {name}"),
                        mode: "tool".into(),
                        session_id: Some(session_id),
                        tool_name: Some("workflow_run".into()),
                        tool_args: Some(args),
                        prompt: None,
                        watch_tool_run_id: None,
                        status: ToolRunStatus::Waiting,
                        created_at: now.clone(),
                    });
                }
                Ok(occurrences)
            })();
            match result {
                Ok(occurrences) => { conn.execute_batch("COMMIT")?; Ok(occurrences) }
                Err(error) => { let _ = conn.execute_batch("ROLLBACK"); Err(error) }
            }
        }).await
    }
}

fn validate_id(workflow_id: &str) -> anyhow::Result<()> {
    anyhow::ensure!(
        is_canonical_id(workflow_id, "wfdef"),
        "invalid workflow_id: expected canonical wfdef- ID"
    );
    Ok(())
}

fn validate_fields(name: &str, description: &str, definition_json: &str) -> anyhow::Result<()> {
    let name = name.trim();
    anyhow::ensure!(
        !name.is_empty()
            && name.chars().count() <= MAX_NAME_CHARS
            && !name.chars().any(char::is_control),
        "Workflow name must contain 1 to 80 printable characters"
    );
    anyhow::ensure!(
        description.chars().count() <= MAX_DESCRIPTION_CHARS
            && !description.chars().any(|character| character == '\0'),
        "Workflow description exceeds its limit"
    );
    anyhow::ensure!(
        definition_json.len() <= MAX_DEFINITION_BYTES,
        "Workflow definition exceeds its size limit"
    );
    let value: serde_json::Value =
        serde_json::from_str(definition_json).context("Workflow definition must be valid JSON")?;
    anyhow::ensure!(
        value.is_object(),
        "Workflow definition must be a JSON object"
    );
    anyhow::ensure!(
        value.get("steps").is_some_and(serde_json::Value::is_array),
        "Workflow definition must contain a steps array"
    );
    Ok(())
}

fn validate_schedule_id(schedule_id: &str) -> anyhow::Result<()> {
    anyhow::ensure!(
        is_canonical_id(schedule_id, "wfsched"),
        "invalid schedule_id: expected canonical wfsched- ID"
    );
    Ok(())
}

fn validate_schedule_fields(
    name: &str,
    workflow_definition_id: Option<&str>,
    workflow_revision: u32,
    definition_json: &str,
    session_id: &str,
    interval_seconds: u64,
    anchor_at: &str,
) -> anyhow::Result<()> {
    anyhow::ensure!(
        !name.trim().is_empty() && name.chars().count() <= MAX_NAME_CHARS,
        "schedule name is invalid"
    );
    if let Some(workflow_definition_id) = workflow_definition_id {
        anyhow::ensure!(
            is_canonical_id(workflow_definition_id, "wfdef"),
            "invalid workflow_definition_id"
        );
    }
    anyhow::ensure!(workflow_revision > 0, "workflow_revision must be positive");
    anyhow::ensure!(is_canonical_id(session_id, "ses"), "invalid session_id");
    anyhow::ensure!(
        (MIN_SCHEDULE_INTERVAL_SECS..=MAX_SCHEDULE_INTERVAL_SECS).contains(&interval_seconds),
        "schedule interval must be between 60 seconds and one year"
    );
    validate_fields(name, "", definition_json)?;
    let parsed = chrono::DateTime::parse_from_rfc3339(anchor_at)
        .context("anchor_at must be an RFC3339 timestamp")?;
    anyhow::ensure!(
        anchor_at
            == parsed
                .with_timezone(&chrono::Utc)
                .to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        "anchor_at must use canonical UTC RFC3339 milliseconds"
    );
    Ok(())
}

fn now_timestamp() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

fn normalize_timestamp(value: &str, field: &str) -> anyhow::Result<String> {
    let timestamp = parse_timestamp(value, field)?;
    Ok(timestamp.to_rfc3339_opts(chrono::SecondsFormat::Millis, true))
}

fn parse_timestamp(value: &str, field: &str) -> anyhow::Result<chrono::DateTime<chrono::Utc>> {
    let timestamp = chrono::DateTime::parse_from_rfc3339(value)
        .with_context(|| format!("invalid {field}: expected RFC3339 timestamp"))?;
    let timestamp = timestamp.with_timezone(&chrono::Utc);
    anyhow::ensure!(
        value == timestamp.to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        "invalid {field}: expected canonical UTC RFC3339 milliseconds"
    );
    Ok(timestamp)
}

fn row_from_sql(row: &rusqlite::Row<'_>) -> rusqlite::Result<WorkflowDefinitionRow> {
    let revision: i64 = row.get(3)?;
    let revision = u32::try_from(revision).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            3,
            rusqlite::types::Type::Integer,
            Box::new(error),
        )
    })?;
    if revision == 0 {
        return Err(rusqlite::Error::FromSqlConversionFailure(
            3,
            rusqlite::types::Type::Integer,
            Box::new(std::io::Error::other("revision must be positive")),
        ));
    }
    Ok(WorkflowDefinitionRow {
        workflow_id: row.get(0)?,
        name: row.get(1)?,
        description: row.get(2)?,
        revision,
        definition_json: row.get(4)?,
        created_at: row.get(5)?,
        updated_at: row.get(6)?,
    })
}

fn schedule_row_from_sql(row: &rusqlite::Row<'_>) -> rusqlite::Result<WorkflowScheduleRow> {
    let revision: i64 = row.get(3)?;
    let interval_seconds: i64 = row.get(6)?;
    let revision = u32::try_from(revision).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            3,
            rusqlite::types::Type::Integer,
            Box::new(error),
        )
    })?;
    let interval_seconds = u64::try_from(interval_seconds).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            6,
            rusqlite::types::Type::Integer,
            Box::new(error),
        )
    })?;
    if revision == 0
        || !(MIN_SCHEDULE_INTERVAL_SECS..=MAX_SCHEDULE_INTERVAL_SECS).contains(&interval_seconds)
    {
        return Err(rusqlite::Error::FromSqlConversionFailure(
            3,
            rusqlite::types::Type::Integer,
            Box::new(std::io::Error::other(
                "invalid schedule revision or interval",
            )),
        ));
    }
    Ok(WorkflowScheduleRow {
        schedule_id: row.get(0)?,
        name: row.get(1)?,
        workflow_definition_id: row.get(2)?,
        workflow_revision: revision,
        definition_json: row.get(4)?,
        session_id: row.get(5)?,
        interval_seconds,
        anchor_at: row.get(7)?,
        next_due_at: row.get(8)?,
        enabled: row.get::<_, i64>(9)? == 1,
        active_tool_run_id: row.get(10)?,
        created_at: row.get(11)?,
        updated_at: row.get(12)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> WorkflowDefinitionStore {
        let db = Arc::new(Database::open_in_memory().unwrap());
        WorkflowDefinitionStore::new(db)
    }

    #[tokio::test]
    async fn definitions_are_versioned_and_deleted_by_canonical_id() {
        let store = store();
        let created = store
            .create(
                "Daily summary".into(),
                "Summarize notes".into(),
                r#"{"steps":[]}"#.into(),
            )
            .await
            .unwrap();
        assert!(is_canonical_id(&created.workflow_id, "wfdef"));
        assert_eq!(created.revision, 1);

        let updated = store
            .update(
                created.workflow_id.clone(),
                created.revision,
                "Daily recap".into(),
                String::new(),
                r#"{"steps":[{"id":"one"}]}"#.into(),
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(updated.revision, 2);
        assert!(
            store
                .update(
                    created.workflow_id.clone(),
                    1,
                    "stale update".into(),
                    String::new(),
                    r#"{"steps":[{"id":"later"}]}"#.into(),
                )
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(store.list().await.unwrap(), [updated]);
        assert!(store.delete(created.workflow_id).await.unwrap());
        assert!(store.list().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn invalid_definitions_and_noncanonical_ids_fail_closed() {
        let store = store();
        assert!(
            store
                .create(" ".into(), String::new(), r#"{"steps":[]}"#.into())
                .await
                .is_err()
        );
        assert!(
            store
                .create("ok".into(), String::new(), r#"{"steps":{}}"#.into())
                .await
                .is_err()
        );
        assert!(store.get("wfdef-short".into()).await.is_err());
    }

    #[tokio::test]
    async fn recurring_occurrence_is_claimed_once_and_skips_missed_ticks() {
        let db = Arc::new(Database::open_in_memory().unwrap());
        let session = db.create_session("scheduled Workflow").unwrap();
        let store = WorkflowDefinitionStore::new(db.clone());
        let now = chrono::Utc::now();
        let anchor = now - chrono::Duration::seconds(125);
        let format = |value: chrono::DateTime<chrono::Utc>| {
            value.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
        };
        let schedule = store
            .create_schedule(CreateWorkflowSchedule {
                name: "Every minute".into(),
                workflow_definition_id: None,
                workflow_revision: 1,
                definition_json: r#"{"steps":[{"id":"one"}]}"#.into(),
                session_id: session.session_id,
                interval_seconds: 60,
                anchor_at: format(anchor),
            })
            .await
            .unwrap();

        let occurrences = store
            .claim_due_occurrences(format(now.clone()))
            .await
            .unwrap();
        assert_eq!(occurrences.len(), 1);
        assert_eq!(occurrences[0].due_at, schedule.anchor_at);
        assert!(
            store
                .claim_due_occurrences(format(now))
                .await
                .unwrap()
                .is_empty()
        );

        let updated_rows = store.list_schedules().await.unwrap();
        assert_eq!(updated_rows.len(), 1);
        let updated = &updated_rows[0];
        assert_eq!(
            updated.active_tool_run_id.as_deref(),
            Some(occurrences[0].tool_run_id.as_str())
        );
        let next_due = parse_timestamp(&updated.next_due_at, "next_due_at").unwrap();
        let persisted_anchor = parse_timestamp(&schedule.anchor_at, "anchor_at").unwrap();
        assert!(next_due > now);
        assert_eq!(
            next_due
                .signed_duration_since(persisted_anchor)
                .num_milliseconds()
                % 60_000,
            0,
            "next occurrence must remain anchored to the original schedule"
        );
        assert_eq!(
            db.get_tool_run(&occurrences[0].tool_run_id)
                .unwrap()
                .unwrap()
                .workflow_schedule_id
                .as_deref(),
            Some(schedule.schedule_id.as_str())
        );
        assert!(store.delete_schedule(schedule.schedule_id).await.unwrap());
        assert!(store.list_schedules().await.unwrap().is_empty());
        assert_eq!(
            db.get_tool_run(&occurrences[0].tool_run_id)
                .unwrap()
                .unwrap()
                .workflow_schedule_id,
            None,
            "deleting a schedule must leave its already-created ToolRun independent"
        );
    }
}
