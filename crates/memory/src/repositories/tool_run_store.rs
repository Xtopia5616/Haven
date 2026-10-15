//! Typed asynchronous persistence boundary for background and scheduled tool_runs.
//!
//! ToolRunService owns ToolRun admission, live state, retry policy, and event
//! publication. This store owns SQLite blocking-pool scheduling and delegates
//! each operation to the ToolRun repositories, including the atomic
//! terminal-row plus completion-outbox transaction for ToolRun results.

use std::sync::Arc;

use crate::Database;
use crate::repositories::tool_run_completion_outbox::ToolRunCompletionOutboxRow;
use crate::repositories::tool_runs::{ScheduledToolRunRow, ToolRunDependencyRow, ToolRunRow};
use anyhow::Context;
use chrono::{DateTime, NaiveDateTime, Utc};
use haven_common::ToolRunCompletionPayload;
use haven_common::lifecycle::ToolRunStatus;
use haven_common::tool_run::ScheduleMode;
use haven_common::types::is_canonical_id;

const MAX_TOOL_RUN_HISTORY_PAGE_SIZE: usize = 200;

fn normalize_tool_run_timestamp(value: &str, field: &str) -> anyhow::Result<String> {
    let timestamp = DateTime::parse_from_rfc3339(value)
        .with_context(|| format!("{field} must be an RFC3339 timestamp"))?;
    Ok(timestamp.with_timezone(&Utc).to_rfc3339())
}

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
    if let Ok(timestamp) = DateTime::parse_from_rfc3339(value) {
        return Ok(timestamp.with_timezone(&Utc).to_rfc3339());
    }

    // SQLite's datetime('now') values are UTC but omit both the separator and
    // offset required by RFC3339. Accept that legacy storage form at reads so
    // callers always receive canonical timestamps.
    let timestamp =
        NaiveDateTime::parse_from_str(value, "%Y-%m-%d %H:%M:%S").with_context(|| {
            format!("invalid persisted {field}: expected RFC3339 or SQLite UTC time")
        })?;
    Ok(timestamp.and_utc().to_rfc3339())
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

fn normalize_stored_due_at(value: &mut String) {
    if !value.trim().is_empty()
        && let Ok(timestamp) = normalize_tool_run_timestamp(value, "due_at")
    {
        *value = timestamp;
    }
    // Invalid waiting due_at values remain readable so the service can move
    // them to terminal history with a quarantine reason.
}

fn normalize_scheduled_due_at(
    due_at: &str,
    watch_tool_run_id: Option<&str>,
) -> anyhow::Result<String> {
    match (due_at.trim().is_empty(), watch_tool_run_id.is_some()) {
        (true, true) => Ok(String::new()),
        (false, false) => normalize_tool_run_timestamp(due_at, "due_at"),
        _ => anyhow::bail!("scheduled ToolRun requires exactly one of due_at or watch_tool_run_id"),
    }
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

fn validate_execution_claim_id(claim_id: &str, tool_run_id: &str) -> anyhow::Result<()> {
    if is_canonical_id(claim_id, "conf") {
        return Ok(());
    }

    anyhow::ensure!(
        claim_id == tool_run_id && is_canonical_id(claim_id, "toolrun"),
        "invalid claim_id: expected canonical conf- ID or the claimed toolrun- ID"
    );
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
    if let Some(due_at) = &mut row.due_at {
        normalize_stored_due_at(due_at);
    }
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

fn validate_scheduled_tool_run_row(row: &ScheduledToolRunRow) -> anyhow::Result<()> {
    validate_id(&row.tool_run_id, "toolrun", "tool_run_id")?;
    validate_optional_id(row.session_id.as_deref(), "ses", "session_id")?;
    validate_optional_id(
        row.watch_tool_run_id.as_deref(),
        "toolrun",
        "watch_tool_run_id",
    )
}

fn normalize_scheduled_tool_run_row(row: &mut ScheduledToolRunRow) -> anyhow::Result<()> {
    validate_scheduled_tool_run_row(row)?;
    normalize_stored_due_at(&mut row.due_at);
    row.created_at = normalize_persisted_tool_run_timestamp(&row.created_at, "created_at")?;
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

    /// Read one persisted ToolRun of either kind.
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

    /// Read only the producer status/result needed by a dependent scheduled
    /// ToolRun. This private projection is not used by UI/history commands.
    pub async fn get_tool_run_dependency(
        &self,
        tool_run_id: String,
    ) -> anyhow::Result<Option<ToolRunDependencyRow>> {
        validate_id(&tool_run_id, "toolrun", "tool_run_id")?;
        self.db
            .run_blocking(move |db| db.get_tool_run_dependency(&tool_run_id))
            .await
    }

    /// Remove a persisted ToolRun of either kind.
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

    /// Move a malformed waiting scheduled row into terminal history.
    pub async fn quarantine_waiting_scheduled_tool_run(
        &self,
        tool_run_id: String,
        error_reason: String,
        finished_at: String,
    ) -> anyhow::Result<bool> {
        validate_id(&tool_run_id, "toolrun", "tool_run_id")?;
        let finished_at = normalize_tool_run_timestamp(&finished_at, "finished_at")?;
        self.db
            .run_blocking(move |db| {
                db.fail_waiting_scheduled_tool_run(&tool_run_id, &error_reason, &finished_at)
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
        command: String,
        started_at: String,
        source_step_id: Option<String>,
    ) -> anyhow::Result<()> {
        validate_id(&tool_run_id, "toolrun", "tool_run_id")?;
        validate_optional_id(session_id.as_deref(), "ses", "session_id")?;
        validate_optional_id(source_step_id.as_deref(), "step", "source_step_id")?;
        let started_at = normalize_tool_run_timestamp(&started_at, "started_at")?;
        self.db
            .run_blocking(move |db| {
                db.save_tool_run_with_source(
                    &tool_run_id,
                    session_id.as_deref(),
                    &command,
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
        exit_code: Option<i32>,
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
                    exit_code,
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

    /// Persist a newly scheduled ToolRun and its durable timer or dependency
    /// trigger relation.
    #[allow(clippy::too_many_arguments)]
    pub async fn save_scheduled_tool_run(
        &self,
        tool_run_id: String,
        due_at: String,
        title: String,
        body: String,
        mode: ScheduleMode,
        session_id: Option<String>,
        tool_name: Option<String>,
        tool_args: Option<String>,
        prompt: Option<String>,
        watch_tool_run_id: Option<String>,
    ) -> anyhow::Result<()> {
        validate_id(&tool_run_id, "toolrun", "tool_run_id")?;
        validate_optional_id(session_id.as_deref(), "ses", "session_id")?;
        validate_optional_id(watch_tool_run_id.as_deref(), "toolrun", "watch_tool_run_id")?;
        let due_at = normalize_scheduled_due_at(&due_at, watch_tool_run_id.as_deref())?;
        self.db
            .run_blocking(move |db| {
                db.save_scheduled_tool_run(
                    &tool_run_id,
                    &due_at,
                    &title,
                    &body,
                    mode,
                    session_id.as_deref(),
                    tool_name.as_deref(),
                    tool_args.as_deref(),
                    prompt.as_deref(),
                    watch_tool_run_id.as_deref(),
                )
            })
            .await
    }

    /// List waiting scheduled tool_runs ordered by due time.
    pub async fn list_pending_scheduled_tool_runs(
        &self,
    ) -> anyhow::Result<Vec<ScheduledToolRunRow>> {
        self.db
            .run_blocking(|db| {
                let mut rows = db.list_pending_scheduled_tool_runs()?;
                for row in &mut rows {
                    normalize_scheduled_tool_run_row(row)?;
                }
                Ok(rows)
            })
            .await
    }

    /// List scheduled rows that have not reached a terminal state, including
    /// triggers already started by another ToolRunService instance.
    pub async fn list_live_scheduled_tool_runs(&self) -> anyhow::Result<Vec<ScheduledToolRunRow>> {
        self.db
            .run_blocking(|db| {
                let mut rows = db.list_live_scheduled_tool_runs()?;
                for row in &mut rows {
                    normalize_scheduled_tool_run_row(row)?;
                }
                Ok(rows)
            })
            .await
    }

    /// Claim a waiting scheduled trigger with a waiting-to-running CAS.
    pub async fn start_scheduled_tool_run(
        &self,
        tool_run_id: String,
        started_at: String,
    ) -> anyhow::Result<bool> {
        validate_id(&tool_run_id, "toolrun", "tool_run_id")?;
        let started_at = normalize_tool_run_timestamp(&started_at, "started_at")?;
        self.db
            .run_blocking(move |db| db.start_scheduled_tool_run(&tool_run_id, &started_at))
            .await
    }

    /// Return an undelivered scheduled fire to its waiting state.
    pub async fn requeue_scheduled_tool_run(&self, tool_run_id: String) -> anyhow::Result<bool> {
        validate_id(&tool_run_id, "toolrun", "tool_run_id")?;
        self.db
            .run_blocking(move |db| db.requeue_scheduled_tool_run(&tool_run_id))
            .await
    }

    /// Persist a running scheduled ToolRun's terminal transition and, for
    /// completed/failed tool mode, its durable ToolRun-result delivery record.
    pub async fn finish_scheduled_tool_run(
        &self,
        tool_run_id: String,
        status: ToolRunStatus,
        result_summary: Option<String>,
        error_reason: Option<String>,
        finished_at: String,
    ) -> anyhow::Result<bool> {
        validate_id(&tool_run_id, "toolrun", "tool_run_id")?;
        let finished_at = normalize_tool_run_timestamp(&finished_at, "finished_at")?;
        self.db
            .run_blocking(move |db| {
                db.finish_scheduled_tool_run(
                    &tool_run_id,
                    status,
                    result_summary.as_deref(),
                    error_reason.as_deref(),
                    &finished_at,
                )
            })
            .await
    }

    /// Claim execution for a pending scheduled ToolRun. A confirmation uses
    /// its `conf-*` request ID; an auto-approved operation uses the ToolRun's
    /// own ID. Repeating with the same claim ID is idempotent, so callers can
    /// retry an approval after a grant write failure without reopening races.
    pub async fn claim_scheduled_tool_run_execution(
        &self,
        tool_run_id: String,
        claim_id: String,
    ) -> anyhow::Result<bool> {
        validate_id(&tool_run_id, "toolrun", "tool_run_id")?;
        validate_execution_claim_id(&claim_id, &tool_run_id)?;
        self.db
            .run_blocking(move |db| db.claim_scheduled_tool_run_execution(&tool_run_id, &claim_id))
            .await
    }

    /// Release an execution claim when a later retryable approval step fails.
    pub async fn release_scheduled_tool_run_execution_claim(
        &self,
        tool_run_id: String,
        claim_id: String,
    ) -> anyhow::Result<bool> {
        validate_id(&tool_run_id, "toolrun", "tool_run_id")?;
        validate_execution_claim_id(&claim_id, &tool_run_id)?;
        self.db
            .run_blocking(move |db| {
                db.release_scheduled_tool_run_execution_claim(&tool_run_id, &claim_id)
            })
            .await
    }

    /// Cancel a waiting or running scheduled ToolRun.
    pub async fn cancel_scheduled_tool_run(
        &self,
        tool_run_id: String,
        finished_at: String,
    ) -> anyhow::Result<bool> {
        validate_id(&tool_run_id, "toolrun", "tool_run_id")?;
        let finished_at = normalize_tool_run_timestamp(&finished_at, "finished_at")?;
        self.db
            .run_blocking(move |db| db.cancel_scheduled_tool_run(&tool_run_id, &finished_at))
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Database;
    use haven_common::types::new_id;

    const STARTED_AT: &str = "2026-09-24T10:00:00Z";
    const FINISHED_AT: &str = "2026-09-24T11:00:00Z";
    const LATE_AT: &str = "2026-09-24T11:01:00Z";

    fn store() -> (Arc<Database>, ToolRunStore) {
        let db = Arc::new(Database::open_in_memory().unwrap());
        let store = ToolRunStore::new(db.clone());
        (db, store)
    }

    fn completion_payload(
        tool_run_id: &str,
        status: ToolRunStatus,
        output: Option<&str>,
        error: Option<&str>,
        error_reason: Option<&str>,
        exit_code: Option<i32>,
        finished_at: &str,
    ) -> ToolRunCompletionPayload {
        ToolRunCompletionPayload {
            tool_run_id: tool_run_id.to_owned(),
            status,
            status_projection_kind: None,
            output: output.map(str::to_owned),
            error: error.map(str::to_owned),
            error_reason: error_reason.map(str::to_owned),
            log_path: None,
            exit_code,
            started_at: Some(STARTED_AT.to_owned()),
            finished_at: Some(finished_at.to_owned()),
            source_step_id: None,
            truncated: false,
        }
    }

    #[tokio::test]
    async fn background_source_step_survives_store_roundtrip() {
        let (_db, store) = store();
        let tool_run_id = new_id("toolrun");
        let session_id = new_id("ses");
        let source_step_id = new_id("step");
        store
            .save_background_tool_run_with_source(
                tool_run_id.clone(),
                Some(session_id),
                "echo source".into(),
                STARTED_AT.into(),
                Some(source_step_id.clone()),
            )
            .await
            .unwrap();

        let row = store.get_tool_run(tool_run_id).await.unwrap().unwrap();
        assert_eq!(row.source_step_id.as_deref(), Some(source_step_id.as_str()));
    }

    #[tokio::test]
    async fn normalizes_tool_run_timestamps_to_utc_before_persistence() {
        let (_db, store) = store();
        let background_id = new_id("toolrun");
        store
            .save_background_tool_run(
                background_id.clone(),
                None,
                "echo normalize".into(),
                "2026-09-24T12:00:00+02:00".into(),
            )
            .await
            .unwrap();
        store
            .finish_background_tool_run(
                background_id.clone(),
                ToolRunStatus::Completed,
                None,
                None,
                None,
                None,
                Some(0),
                "2026-09-24T13:00:00+02:00".into(),
            )
            .await
            .unwrap();

        let row = store.get_tool_run(background_id).await.unwrap().unwrap();
        assert_eq!(row.started_at.as_deref(), Some("2026-09-24T10:00:00+00:00"));
        assert_eq!(
            row.finished_at.as_deref(),
            Some("2026-09-24T11:00:00+00:00")
        );
        assert!(DateTime::parse_from_rfc3339(&row.created_at).is_ok());

        let scheduled_id = new_id("toolrun");
        store
            .save_scheduled_tool_run(
                scheduled_id.clone(),
                "2026-09-25T12:00:00+02:00".into(),
                "Reminder".into(),
                "body".into(),
                ScheduleMode::Tool,
                None,
                None,
                None,
                None,
                None,
            )
            .await
            .unwrap();
        let due_at = store
            .list_pending_scheduled_tool_runs()
            .await
            .unwrap()
            .into_iter()
            .find(|row| row.tool_run_id == scheduled_id)
            .unwrap()
            .due_at;
        assert_eq!(due_at, "2026-09-25T10:00:00+00:00");
    }

    #[tokio::test]
    async fn rejects_invalid_tool_run_timestamps_and_normalizes_watch_due_at() {
        let (db, store) = store();
        let background_id = new_id("toolrun");
        assert!(
            store
                .save_background_tool_run(
                    background_id,
                    None,
                    "echo invalid timestamp".into(),
                    "not a timestamp".into(),
                )
                .await
                .is_err()
        );
        assert!(db.list_tool_runs(None).unwrap().is_empty());

        let scheduled_id = new_id("toolrun");
        assert!(
            store
                .save_scheduled_tool_run(
                    scheduled_id.clone(),
                    "not a timestamp".into(),
                    "Reminder".into(),
                    "body".into(),
                    ScheduleMode::Tool,
                    None,
                    None,
                    None,
                    None,
                    None,
                )
                .await
                .is_err()
        );
        assert!(db.list_tool_runs(None).unwrap().is_empty());

        let watched_id = new_id("toolrun");
        store
            .save_scheduled_tool_run(
                watched_id.clone(),
                "  ".into(),
                "Follow up".into(),
                "body".into(),
                haven_common::tool_run::ScheduleMode::Continue,
                None,
                None,
                None,
                Some("prompt".into()),
                Some(new_id("toolrun")),
            )
            .await
            .unwrap();
        let watched = store
            .list_pending_scheduled_tool_runs()
            .await
            .unwrap()
            .into_iter()
            .find(|row| row.tool_run_id == watched_id)
            .unwrap();
        assert_eq!(watched.due_at, "");
    }

    #[tokio::test]
    async fn normalizes_legacy_sqlite_timestamps_when_reading_tool_runs() {
        let (db, store) = store();
        let tool_run_id = new_id("toolrun");
        db.save_tool_run(&tool_run_id, None, "echo legacy", STARTED_AT)
            .unwrap();
        db.conn()
            .execute(
                "UPDATE tool_runs SET started_at = ?1, finished_at = ?2 WHERE id = ?3",
                rusqlite::params!["2026-09-24 10:00:00", "2026-09-24 11:00:00", tool_run_id],
            )
            .unwrap();

        let row = store.get_tool_run(tool_run_id).await.unwrap().unwrap();
        assert_eq!(row.started_at.as_deref(), Some("2026-09-24T10:00:00+00:00"));
        assert_eq!(
            row.finished_at.as_deref(),
            Some("2026-09-24T11:00:00+00:00")
        );
        assert!(DateTime::parse_from_rfc3339(&row.created_at).is_ok());
    }

    #[tokio::test]
    async fn terminal_history_page_size_is_bounded_by_memory_store() {
        let (db, store) = store();
        for index in 0..(MAX_TOOL_RUN_HISTORY_PAGE_SIZE + 5) {
            let tool_run_id = new_id("toolrun");
            let started_at = format!("2026-09-24T10:{:02}:00Z", index % 60);
            db.save_tool_run(&tool_run_id, None, "echo history", &started_at)
                .unwrap();
            db.finish_tool_run(
                &tool_run_id,
                ToolRunStatus::Completed,
                Some("done"),
                None,
                None,
                None,
                Some(0),
                "2026-09-24T11:00:00Z",
            )
            .unwrap();
        }

        let rows = store
            .list_terminal_tool_runs(None, None, usize::MAX)
            .await
            .unwrap();

        assert_eq!(rows.len(), MAX_TOOL_RUN_HISTORY_PAGE_SIZE);
    }

    #[tokio::test]
    async fn completion_claim_and_ack_preserve_durable_delivery_boundary() {
        let (_db, store) = store();
        let tool_run_id = new_id("toolrun");
        let session_id = new_id("ses");
        store
            .save_background_tool_run(
                tool_run_id.clone(),
                Some(session_id.clone()),
                "echo result".into(),
                STARTED_AT.into(),
            )
            .await
            .unwrap();

        let mut payload = completion_payload(
            &tool_run_id,
            ToolRunStatus::Completed,
            Some("durable result"),
            None,
            None,
            Some(0),
            "2026-09-24T13:00:00+02:00",
        );
        payload.started_at = Some("2026-09-24T12:00:00+02:00".into());
        assert!(
            store
                .finish_background_tool_run_with_completion(payload)
                .await
                .unwrap()
        );

        let claimed = store.claim_pending_completion().await.unwrap().unwrap();
        assert_eq!(claimed.tool_run_id, tool_run_id);
        assert_eq!(claimed.session_id.as_deref(), Some(session_id.as_str()));
        assert_eq!(claimed.payload.output.as_deref(), Some("durable result"));
        assert_eq!(
            claimed.payload.started_at.as_deref(),
            Some("2026-09-24T10:00:00+00:00")
        );
        assert_eq!(
            claimed.payload.finished_at.as_deref(),
            Some("2026-09-24T11:00:00+00:00")
        );
        // The unexpired claim prevents another consumer from claiming it.
        assert!(store.claim_pending_completion().await.unwrap().is_none());
        assert!(
            store
                .acknowledge_completion(claimed.tool_run_result_id)
                .await
                .unwrap()
        );
        assert!(store.claim_pending_completion().await.unwrap().is_none());
    }

    #[tokio::test]
    async fn background_terminal_cas_enqueues_only_the_winning_snapshot() {
        let (_db, store) = store();
        let tool_run_id = new_id("toolrun");
        store
            .save_background_tool_run(
                tool_run_id.clone(),
                None,
                "echo winner".into(),
                STARTED_AT.into(),
            )
            .await
            .unwrap();

        assert!(
            store
                .finish_background_tool_run_with_completion(completion_payload(
                    &tool_run_id,
                    ToolRunStatus::Completed,
                    Some("winner"),
                    None,
                    None,
                    Some(0),
                    FINISHED_AT,
                ),)
                .await
                .unwrap()
        );
        assert!(
            !store
                .finish_background_tool_run_with_completion(completion_payload(
                    &tool_run_id,
                    ToolRunStatus::Failed,
                    None,
                    Some("late"),
                    Some("late"),
                    Some(1),
                    LATE_AT,
                ),)
                .await
                .unwrap()
        );

        let row = store.get_tool_run(tool_run_id).await.unwrap().unwrap();
        assert_eq!(row.status, ToolRunStatus::Completed);
        assert_eq!(row.output.as_deref(), Some("winner"));
        let completion = store.claim_pending_completion().await.unwrap().unwrap();
        assert_eq!(completion.status, row.status);
        assert_eq!(completion.payload.output.as_deref(), Some("winner"));
    }

    #[tokio::test]
    async fn scheduled_claim_terminal_and_quarantine_are_conditional() {
        let (db, store) = store();
        let tool_run_id = new_id("toolrun");
        store
            .save_scheduled_tool_run(
                tool_run_id.clone(),
                "2026-09-25T10:00:00Z".into(),
                "Reminder".into(),
                "body".into(),
                ScheduleMode::Tool,
                None,
                None,
                None,
                None,
                None,
            )
            .await
            .unwrap();
        assert!(
            store
                .start_scheduled_tool_run(tool_run_id.clone(), STARTED_AT.into())
                .await
                .unwrap()
        );
        assert!(
            !store
                .start_scheduled_tool_run(tool_run_id.clone(), LATE_AT.into())
                .await
                .unwrap()
        );
        assert!(
            store
                .finish_scheduled_tool_run(
                    tool_run_id.clone(),
                    ToolRunStatus::Completed,
                    Some("tool result".into()),
                    None,
                    FINISHED_AT.into(),
                )
                .await
                .unwrap()
        );
        assert!(
            !store
                .finish_scheduled_tool_run(
                    tool_run_id.clone(),
                    ToolRunStatus::Failed,
                    None,
                    Some("late".into()),
                    LATE_AT.into(),
                )
                .await
                .unwrap()
        );
        assert!(
            store
                .list_pending_scheduled_tool_runs()
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            store
                .get_tool_run(tool_run_id.clone())
                .await
                .unwrap()
                .unwrap()
                .status,
            ToolRunStatus::Completed
        );
        let dependency = store
            .get_tool_run_dependency(tool_run_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(dependency.status, ToolRunStatus::Completed);
        assert_eq!(dependency.result.as_deref(), Some("tool result"));

        let invalid_tool_run_id = new_id("toolrun");
        db.save_scheduled_tool_run(
            &invalid_tool_run_id,
            "invalid",
            "Reminder",
            "bad",
            ScheduleMode::Tool,
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();
        assert!(
            store
                .quarantine_waiting_scheduled_tool_run(
                    invalid_tool_run_id.clone(),
                    "invalid due_at".into(),
                    FINISHED_AT.into(),
                )
                .await
                .unwrap()
        );
        assert!(
            !store
                .quarantine_waiting_scheduled_tool_run(
                    invalid_tool_run_id.clone(),
                    "late quarantine".into(),
                    LATE_AT.into(),
                )
                .await
                .unwrap()
        );
        assert_eq!(
            store
                .get_tool_run(invalid_tool_run_id)
                .await
                .unwrap()
                .unwrap()
                .error_reason
                .as_deref(),
            Some("invalid due_at")
        );
    }

    #[tokio::test]
    async fn rejects_noncanonical_tool_run_write_ids_before_persistence() {
        let (db, store) = store();
        assert!(
            store
                .save_background_tool_run(
                    "toolrun-short".into(),
                    None,
                    "echo invalid".into(),
                    STARTED_AT.into(),
                )
                .await
                .is_err()
        );

        let tool_run_id = new_id("toolrun");
        assert!(
            store
                .save_background_tool_run_with_source(
                    tool_run_id.clone(),
                    Some("ses-short".into()),
                    "echo invalid".into(),
                    STARTED_AT.into(),
                    Some("step-short".into()),
                )
                .await
                .is_err()
        );
        assert!(
            store
                .save_scheduled_tool_run(
                    tool_run_id,
                    "due".into(),
                    "title".into(),
                    "body".into(),
                    haven_common::tool_run::ScheduleMode::Continue,
                    None,
                    None,
                    None,
                    None,
                    Some("toolrun-watch".into()),
                )
                .await
                .is_err()
        );
        assert_eq!(db.list_tool_runs(None).unwrap().len(), 0);
    }

    #[tokio::test]
    async fn rejects_invalid_claim_ids_and_malformed_read_rows() {
        let (db, store) = store();
        let tool_run_id = new_id("toolrun");
        db.save_tool_run(&tool_run_id, Some("ses-short"), "echo", STARTED_AT)
            .unwrap();
        assert!(store.get_tool_run(tool_run_id.clone()).await.is_err());

        let scheduled_id = new_id("toolrun");
        db.save_scheduled_tool_run(
            &scheduled_id,
            "",
            "title",
            "body",
            haven_common::tool_run::ScheduleMode::Continue,
            None,
            None,
            None,
            Some("prompt"),
            Some("toolrun-short"),
        )
        .unwrap();
        assert!(store.list_pending_scheduled_tool_runs().await.is_err());

        assert!(
            store
                .claim_scheduled_tool_run_execution(new_id("toolrun"), "claim-short".into())
                .await
                .is_err()
        );
        assert!(
            store
                .claim_scheduled_tool_run_execution(new_id("toolrun"), new_id("toolrun"))
                .await
                .is_err()
        );
        assert!(
            !store
                .claim_scheduled_tool_run_execution(new_id("toolrun"), new_id("conf"))
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn scheduled_execution_claim_accepts_only_its_own_tool_run_id_for_auto_approval() {
        let (db, store) = store();
        let tool_run_id = new_id("toolrun");
        store
            .save_scheduled_tool_run(
                tool_run_id.clone(),
                "2026-09-25T10:00:00Z".into(),
                "Reminder".into(),
                "body".into(),
                ScheduleMode::Tool,
                None,
                None,
                None,
                None,
                None,
            )
            .await
            .unwrap();
        assert!(
            store
                .start_scheduled_tool_run(tool_run_id.clone(), STARTED_AT.into())
                .await
                .unwrap()
        );

        assert!(
            store
                .claim_scheduled_tool_run_execution(tool_run_id.clone(), new_id("toolrun"))
                .await
                .is_err()
        );
        assert_eq!(
            db.get_kv(&format!("scheduled_execution_claim.{tool_run_id}"))
                .unwrap(),
            None
        );

        assert!(
            store
                .claim_scheduled_tool_run_execution(tool_run_id.clone(), tool_run_id.clone())
                .await
                .unwrap()
        );
        assert_eq!(
            db.get_kv(&format!("scheduled_execution_claim.{tool_run_id}"))
                .unwrap()
                .as_deref(),
            Some(tool_run_id.as_str())
        );
    }

    #[tokio::test]
    async fn rejects_completion_payload_identity_mismatch_before_delivery() {
        let (db, store) = store();
        let tool_run_id = new_id("toolrun");
        db.save_tool_run(&tool_run_id, None, "echo", STARTED_AT)
            .unwrap();
        assert!(
            db.finish_tool_run_with_completion(&completion_payload(
                &tool_run_id,
                ToolRunStatus::Completed,
                Some("done"),
                None,
                None,
                Some(0),
                FINISHED_AT,
            ))
            .unwrap()
        );
        let mismatched_payload = serde_json::to_string(&completion_payload(
            &new_id("toolrun"),
            ToolRunStatus::Completed,
            Some("done"),
            None,
            None,
            Some(0),
            FINISHED_AT,
        ))
        .unwrap();
        db.conn()
            .execute(
                "UPDATE tool_run_completion_outbox SET status_json = ?1 WHERE tool_run_id = ?2",
                rusqlite::params![mismatched_payload, tool_run_id],
            )
            .unwrap();

        assert!(store.claim_pending_completion().await.is_err());
    }
}
