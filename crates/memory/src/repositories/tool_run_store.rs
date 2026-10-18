//! Typed asynchronous persistence boundary for background ToolRuns.
//!
//! ToolRunService owns ToolRun admission, live state, retry policy, and event
//! publication. This store owns SQLite blocking-pool scheduling and delegates
//! each operation to the ToolRun repositories, including the atomic
//! terminal-row plus completion-outbox transaction for ToolRun results.

use std::sync::Arc;

use crate::Database;
use crate::repositories::tool_run_completion_outbox::ToolRunCompletionOutboxRow;
use crate::repositories::tool_runs::{ToolRunRow, normalize_tool_run_timestamp};
use anyhow::Context;
use chrono::{DateTime, SecondsFormat, Utc};
use haven_common::ToolRunCompletionPayload;
use haven_common::lifecycle::ToolRunStatus;
use haven_common::types::is_canonical_id;

const MAX_TOOL_RUN_HISTORY_PAGE_SIZE: usize = 200;

fn normalize_optional_tool_run_timestamp(
    value: Option<String>,
    field: &str,
) -> anyhow::Result<Option<String>> {
    value
        .as_deref()
        .map(|value| normalize_tool_run_timestamp(value, field))
        .transpose()
}

fn normalize_persisted_tool_run_timestamp(value: &str, field: &str) -> anyhow::Result<String> {
    let timestamp = DateTime::parse_from_rfc3339(value)
        .with_context(|| format!("invalid persisted {field}: expected RFC3339 timestamp"))?;
    let canonical = timestamp
        .with_timezone(&Utc)
        .to_rfc3339_opts(SecondsFormat::Millis, true);
    anyhow::ensure!(
        value == canonical,
        "invalid persisted {field}: expected canonical UTC RFC3339 milliseconds"
    );
    Ok(canonical)
}

fn normalize_optional_persisted_tool_run_timestamp(
    value: Option<String>,
    field: &str,
) -> anyhow::Result<Option<String>> {
    value
        .as_deref()
        .map(|value| normalize_persisted_tool_run_timestamp(value, field))
        .transpose()
}

fn validate_id(id: &str, prefix: &str, field: &str) -> anyhow::Result<()> {
    anyhow::ensure!(
        is_canonical_id(id, prefix),
        "invalid {field}: expected canonical {prefix}- ID"
    );
    Ok(())
}

fn validate_optional_id(id: Option<&str>, prefix: &str, field: &str) -> anyhow::Result<()> {
    if let Some(id) = id {
        validate_id(id, prefix, field)?;
    }
    Ok(())
}

fn validate_tool_run_row(row: &ToolRunRow) -> anyhow::Result<()> {
    validate_id(&row.tool_run_id, "toolrun", "tool_run_id")?;
    validate_optional_id(row.session_id.as_deref(), "ses", "session_id")?;
    validate_optional_id(row.source_step_id.as_deref(), "step", "source_step_id")
}

fn validate_tool_run_rows(rows: &[ToolRunRow]) -> anyhow::Result<()> {
    for row in rows {
        validate_tool_run_row(row)?;
    }
    Ok(())
}

fn normalize_tool_run_row(row: &mut ToolRunRow) -> anyhow::Result<()> {
    validate_tool_run_row(row)?;
    row.started_at =
        normalize_optional_persisted_tool_run_timestamp(row.started_at.take(), "started_at")?;
    row.finished_at =
        normalize_optional_persisted_tool_run_timestamp(row.finished_at.take(), "finished_at")?;
    row.created_at = normalize_persisted_tool_run_timestamp(&row.created_at, "created_at")?;
    Ok(())
}

fn normalize_tool_run_rows(rows: &mut [ToolRunRow]) -> anyhow::Result<()> {
    for row in rows {
        normalize_tool_run_row(row)?;
    }
    Ok(())
}

fn normalize_completion_outbox_row(row: &mut ToolRunCompletionOutboxRow) -> anyhow::Result<()> {
    validate_id(&row.tool_run_id, "toolrun", "tool_run_id")?;
    validate_id(&row.tool_run_result_id, "toolrun", "tool_run_result_id")?;
    anyhow::ensure!(
        row.tool_run_result_id == row.tool_run_id,
        "tool_run_result_id does not match tool_run_id"
    );
    validate_optional_id(row.session_id.as_deref(), "ses", "session_id")?;
    validate_id(&row.payload.tool_run_id, "toolrun", "payload.tool_run_id")?;
    anyhow::ensure!(
        row.payload.tool_run_id == row.tool_run_id,
        "payload.tool_run_id does not match tool_run_id"
    );
    validate_optional_id(
        row.payload.source_step_id.as_deref(),
        "step",
        "payload.source_step_id",
    )?;
    row.payload.started_at = normalize_optional_persisted_tool_run_timestamp(
        row.payload.started_at.take(),
        "payload.started_at",
    )?;
    row.payload.finished_at = normalize_optional_persisted_tool_run_timestamp(
        row.payload.finished_at.take(),
        "payload.finished_at",
    )?;
    Ok(())
}

#[derive(Clone)]
pub struct ToolRunStore {
    db: Arc<Database>,
}

impl ToolRunStore {
    #[cfg(feature = "test-support")]
    pub fn new(db: Arc<Database>) -> Self {
        Self::from_database(db)
    }

    #[cfg(not(feature = "test-support"))]
    pub(crate) fn new(db: Arc<Database>) -> Self {
        Self::from_database(db)
    }

    fn from_database(db: Arc<Database>) -> Self {
        Self { db }
    }

    /// Claim the oldest undelivered completion, reconciling missing outbox
    /// records from terminal ToolRun history as part of the claim operation.
    pub async fn claim_pending_completion(
        &self,
    ) -> anyhow::Result<Option<ToolRunCompletionOutboxRow>> {
        self.db
            .run_blocking(|db| {
                let mut row = db.claim_tool_run_completion()?;
                if let Some(row) = &mut row {
                    normalize_completion_outbox_row(row)?;
                }
                Ok(row)
            })
            .await
    }

    /// Acknowledge a completion after its transcript projection is durable.
    pub async fn acknowledge_completion(&self, tool_run_result_id: String) -> anyhow::Result<bool> {
        validate_id(&tool_run_result_id, "toolrun", "tool_run_result_id")?;
        self.db
            .run_blocking(move |db| db.acknowledge_tool_run_completion(&tool_run_result_id))
            .await
    }

    /// Acknowledge a completion that has no session owner only if it remains
    /// unowned at the SQLite write boundary.
    pub async fn acknowledge_unowned_completion(
        &self,
        tool_run_result_id: String,
    ) -> anyhow::Result<bool> {
        validate_id(&tool_run_result_id, "toolrun", "tool_run_result_id")?;
        self.db
            .run_blocking(move |db| db.acknowledge_unowned_tool_run_completion(&tool_run_result_id))
            .await
    }

    /// Read persisted tool_runs owned by one session, newest first, optionally
    /// filtered by kind.
    pub async fn list_tool_runs_for_session(
        &self,
        session_id: String,
        kind: Option<String>,
    ) -> anyhow::Result<Vec<ToolRunRow>> {
        validate_id(&session_id, "ses", "session_id")?;
        self.db
            .run_blocking(move |db| {
                let mut rows =
                    db.list_tool_runs_for_session(kind.as_deref(), Some(session_id.as_str()))?;
                normalize_tool_run_rows(&mut rows)?;
                Ok(rows)
            })
            .await
    }

    /// Read terminal ToolRun history, optionally filtered by kind and session.
    /// A missing limit is reserved for board hydration; user-requested history
    /// pages should always provide the bounded value chosen by their owner.
    pub async fn list_terminal_tool_runs(
        &self,
        kind: Option<String>,
        session_id: Option<String>,
        limit: usize,
    ) -> anyhow::Result<Vec<ToolRunRow>> {
        validate_optional_id(session_id.as_deref(), "ses", "session_id")?;
        let limit = limit.min(MAX_TOOL_RUN_HISTORY_PAGE_SIZE);
        self.db
            .run_blocking(move |db| {
                let mut rows = db.list_terminal_tool_runs(
                    kind.as_deref(),
                    session_id.as_deref(),
                    Some(limit),
                )?;
                normalize_tool_run_rows(&mut rows)?;
                Ok(rows)
            })
            .await
    }

    /// Read all terminal background ToolRuns for board hydration. This explicit
    /// query is intentionally separate from bounded history pages.
    pub async fn list_all_terminal_background_tool_runs(&self) -> anyhow::Result<Vec<ToolRunRow>> {
        self.db
            .run_blocking(|db| {
                let mut rows = db.list_terminal_tool_runs(Some("background"), None, None)?;
                normalize_tool_run_rows(&mut rows)?;
                Ok(rows)
            })
            .await
    }

    /// Return ToolRun IDs whose durable rows still reference full-output logs.
    pub async fn list_tool_run_output_log_ids(&self) -> anyhow::Result<Vec<String>> {
        self.db
            .run_blocking(|db| {
                let ids = db.list_tool_run_output_log_ids()?;
                for tool_run_id in &ids {
                    validate_id(tool_run_id, "toolrun", "tool_run_id")?;
                }
                Ok(ids)
            })
            .await
    }

    /// Read one persisted background ToolRun.
    pub async fn get_tool_run(&self, tool_run_id: String) -> anyhow::Result<Option<ToolRunRow>> {
        validate_id(&tool_run_id, "toolrun", "tool_run_id")?;
        self.db
            .run_blocking(move |db| {
                let mut row = db.get_tool_run(&tool_run_id)?;
                if let Some(row) = &mut row {
                    normalize_tool_run_row(row)?;
                }
                Ok(row)
            })
            .await
    }

    /// Remove a persisted background ToolRun.
    pub async fn delete_tool_run(&self, tool_run_id: String) -> anyhow::Result<bool> {
        validate_id(&tool_run_id, "toolrun", "tool_run_id")?;
        self.db
            .run_blocking(move |db| db.delete_tool_run(&tool_run_id))
            .await
    }

    /// Atomically delete terminal history while retaining pending/running
    /// work and undelivered completion results.
    pub async fn clear_terminal_tool_runs(&self) -> anyhow::Result<Vec<String>> {
        self.db
            .run_blocking(|db| {
                let rows = db.list_terminal_tool_runs(None, None, None)?;
                validate_tool_run_rows(&rows)?;
                let deleted = db.clear_terminal_tool_runs()?;
                for tool_run_id in &deleted {
                    validate_id(tool_run_id, "toolrun", "tool_run_id")?;
                }
                Ok(deleted)
            })
            .await
    }

    /// Mark ToolRun rows left running by a previous process as failed.
    pub async fn mark_interrupted_tool_runs(&self) -> anyhow::Result<usize> {
        self.db
            .run_blocking(|db| db.mark_interrupted_tool_runs())
            .await
    }

    /// Persist a newly spawned background ToolRun.
    pub async fn save_background_tool_run(
        &self,
        tool_run_id: String,
        session_id: Option<String>,
        command: String,
        started_at: String,
    ) -> anyhow::Result<()> {
        self.save_background_tool_run_with_source(
            tool_run_id,
            session_id,
            command,
            started_at,
            None,
        )
        .await
    }

    /// Persist a newly spawned background ToolRun with its originating Agent
    /// tool step, if the ToolRun came from a session tool invocation.
    pub async fn save_background_tool_run_with_source(
        &self,
        tool_run_id: String,
        session_id: Option<String>,
        _command: String,
        started_at: String,
        source_step_id: Option<String>,
    ) -> anyhow::Result<()> {
        self.save_background_tool_run_with_metadata(
            tool_run_id,
            "shell".into(),
            session_id,
            started_at,
            source_step_id,
        )
        .await
    }

    /// Persist a newly spawned invocation of any registered tool.
    pub async fn save_background_tool_run_with_metadata(
        &self,
        tool_run_id: String,
        tool_name: String,
        session_id: Option<String>,
        started_at: String,
        source_step_id: Option<String>,
    ) -> anyhow::Result<()> {
        validate_id(&tool_run_id, "toolrun", "tool_run_id")?;
        anyhow::ensure!(
            !tool_name.trim().is_empty() && tool_name.len() <= 128,
            "invalid ToolRun tool_name"
        );
        validate_optional_id(session_id.as_deref(), "ses", "session_id")?;
        validate_optional_id(source_step_id.as_deref(), "step", "source_step_id")?;
        let started_at = normalize_tool_run_timestamp(&started_at, "started_at")?;
        self.db
            .run_blocking(move |db| {
                db.save_tool_run_with_metadata(
                    &tool_run_id,
                    &tool_name,
                    session_id.as_deref(),
                    &started_at,
                    source_step_id.as_deref(),
                )
            })
            .await
    }

    /// Bind a background ToolRun to its owning session and any pending outbox
    /// completion in one repository transaction.
    pub async fn bind_background_tool_run_session(
        &self,
        tool_run_id: String,
        session_id: String,
    ) -> anyhow::Result<()> {
        validate_id(&tool_run_id, "toolrun", "tool_run_id")?;
        validate_id(&session_id, "ses", "session_id")?;
        self.db
            .run_blocking(move |db| db.update_tool_run_session(&tool_run_id, &session_id))
            .await
    }

    /// Persist a background ToolRun terminal row without an agent completion.
    /// This is used only by registration compensation after process admission
    /// fails.
    #[allow(clippy::too_many_arguments)]
    pub async fn finish_background_tool_run(
        &self,
        tool_run_id: String,
        status: ToolRunStatus,
        output: Option<String>,
        error: Option<String>,
        error_reason: Option<String>,
        log_path: Option<String>,
        finished_at: String,
    ) -> anyhow::Result<bool> {
        validate_id(&tool_run_id, "toolrun", "tool_run_id")?;
        let finished_at = normalize_tool_run_timestamp(&finished_at, "finished_at")?;
        self.db
            .run_blocking(move |db| {
                db.finish_tool_run(
                    &tool_run_id,
                    status,
                    output.as_deref(),
                    error.as_deref(),
                    error_reason.as_deref(),
                    log_path.as_deref(),
                    &finished_at,
                )
            })
            .await
    }

    /// Persist cancellation of a running background ToolRun.
    pub async fn cancel_background_tool_run(
        &self,
        tool_run_id: String,
        finished_at: String,
    ) -> anyhow::Result<bool> {
        validate_id(&tool_run_id, "toolrun", "tool_run_id")?;
        let finished_at = normalize_tool_run_timestamp(&finished_at, "finished_at")?;
        self.db
            .run_blocking(move |db| db.cancel_background_tool_run(&tool_run_id, &finished_at))
            .await
    }

    /// Commit a completed/failed background row and its completion outbox
    /// record atomically. The outbox is acknowledged by the caller only after
    /// durable transcript projection.
    pub async fn finish_background_tool_run_with_completion(
        &self,
        mut payload: ToolRunCompletionPayload,
    ) -> anyhow::Result<bool> {
        validate_id(&payload.tool_run_id, "toolrun", "tool_run_id")?;
        anyhow::ensure!(
            !payload.tool_name.trim().is_empty() && payload.tool_name.len() <= 128,
            "invalid background ToolRun tool_name"
        );
        validate_optional_id(payload.source_step_id.as_deref(), "step", "source_step_id")?;
        payload.started_at =
            normalize_optional_tool_run_timestamp(payload.started_at, "started_at")?;
        payload.finished_at = Some(normalize_tool_run_timestamp(
            payload
                .finished_at
                .as_deref()
                .context("terminal ToolRun completion requires finished_at")?,
            "finished_at",
        )?);
        self.db
            .run_blocking(move |db| db.finish_tool_run_with_completion(&payload))
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Database;
    use haven_common::types::new_id;

    const STARTED_AT: &str = "2026-09-24T10:00:00.000Z";
    const FINISHED_AT: &str = "2026-09-24T11:00:00.000Z";

    fn store() -> (Arc<Database>, ToolRunStore) {
        let db = Arc::new(Database::open_in_memory().unwrap());
        let store = ToolRunStore::new(db.clone());
        (db, store)
    }

    #[tokio::test]
    async fn background_tool_run_roundtrips_with_canonical_ids_and_times() {
        let (_db, store) = store();
        let tool_run_id = new_id("toolrun");
        let session_id = new_id("ses");
        let source_step_id = new_id("step");
        store
            .save_background_tool_run_with_source(
                tool_run_id.clone(),
                Some(session_id.clone()),
                "echo ok".into(),
                "2026-09-24T12:00:00+02:00".into(),
                Some(source_step_id.clone()),
            )
            .await
            .unwrap();
        let row = store.get_tool_run(tool_run_id).await.unwrap().unwrap();
        assert_eq!(row.kind, "background");
        assert_eq!(row.session_id.as_deref(), Some(session_id.as_str()));
        assert_eq!(row.source_step_id.as_deref(), Some(source_step_id.as_str()));
        assert_eq!(row.started_at.as_deref(), Some(STARTED_AT));
        assert_eq!(row.tool_name, "shell");
    }

    #[tokio::test]
    async fn completion_is_acknowledged_only_after_durable_result_claim() {
        let (_db, store) = store();
        let tool_run_id = new_id("toolrun");
        store
            .save_background_tool_run(
                tool_run_id.clone(),
                None,
                "echo ok".into(),
                STARTED_AT.into(),
            )
            .await
            .unwrap();
        let payload = ToolRunCompletionPayload {
            tool_run_id: tool_run_id.clone(),
            tool_name: "shell".into(),
            status: ToolRunStatus::Completed,
            output: Some("ok".into()),
            error: None,
            error_reason: None,
            log_path: None,
            started_at: Some(STARTED_AT.into()),
            finished_at: Some(FINISHED_AT.into()),
            source_step_id: None,
            truncated: false,
        };
        assert!(
            store
                .finish_background_tool_run_with_completion(payload)
                .await
                .unwrap()
        );
        let completion = store.claim_pending_completion().await.unwrap().unwrap();
        assert_eq!(completion.tool_run_id, tool_run_id);
        assert!(
            store
                .acknowledge_unowned_completion(completion.tool_run_result_id.clone())
                .await
                .unwrap()
        );
        assert!(store.claim_pending_completion().await.unwrap().is_none());
    }

    #[tokio::test]
    async fn rejects_noncanonical_ids_and_invalid_timestamps() {
        let (db, store) = store();
        assert!(
            store
                .save_background_tool_run(
                    "toolrun-invalid".into(),
                    None,
                    "echo".into(),
                    STARTED_AT.into()
                )
                .await
                .is_err()
        );
        let valid_id = new_id("toolrun");
        assert!(
            store
                .save_background_tool_run(valid_id, None, "echo".into(), "not-a-timestamp".into())
                .await
                .is_err()
        );
        assert!(db.list_tool_runs(None).unwrap().is_empty());
    }
}
