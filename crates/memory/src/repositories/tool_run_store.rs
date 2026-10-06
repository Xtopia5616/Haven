//! Typed asynchronous persistence boundary for background and scheduled tool_runs.
//!
//! ToolRunService owns ToolRun admission, live state, retry policy, and event
//! publication. This store owns SQLite blocking-pool scheduling and delegates
//! each operation to the ToolRun repositories, including the atomic
//! terminal-row plus completion-outbox transaction for ToolRun results.

use std::sync::Arc;

use crate::Database;
use crate::repositories::scheduled_tool_runs::{
    ScheduledToolRunRow, ToolRunDependencyRow, ToolRunRow,
};
use crate::repositories::tool_run_completion_outbox::ToolRunCompletionOutboxRow;
use haven_common::ToolRunStatus;
use serde_json::Value;

#[derive(Clone)]
pub struct ToolRunStore {
    db: Arc<Database>,
}

impl ToolRunStore {
    pub fn new(db: Arc<Database>) -> Self {
        Self { db }
    }

    /// Claim the oldest undelivered completion, reconciling missing outbox
    /// records from terminal ToolRun history as part of the claim operation.
    pub async fn claim_pending_completion(
        &self,
    ) -> anyhow::Result<Option<ToolRunCompletionOutboxRow>> {
        self.db
            .run_blocking(|db| db.claim_tool_run_completion())
            .await
    }

    /// Acknowledge a completion after its transcript projection is durable.
    pub async fn acknowledge_completion(&self, tool_run_result_id: String) -> anyhow::Result<bool> {
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
        self.db
            .run_blocking(move |db| db.acknowledge_unowned_tool_run_completion(&tool_run_result_id))
            .await
    }

    /// Read persisted tool_runs, newest first, optionally filtered by kind.
    pub async fn list_tool_runs(&self, kind: Option<String>) -> anyhow::Result<Vec<ToolRunRow>> {
        self.db
            .run_blocking(move |db| db.list_tool_runs(kind.as_deref()))
            .await
    }

    /// Read persisted tool_runs owned by one session, newest first, optionally
    /// filtered by kind.
    pub async fn list_tool_runs_for_session(
        &self,
        session_id: String,
        kind: Option<String>,
    ) -> anyhow::Result<Vec<ToolRunRow>> {
        self.db
            .run_blocking(move |db| {
                db.list_tool_runs_for_session(kind.as_deref(), Some(session_id.as_str()))
            })
            .await
    }

    /// Read one persisted ToolRun of either kind.
    pub async fn get_tool_run(&self, tool_run_id: String) -> anyhow::Result<Option<ToolRunRow>> {
        self.db
            .run_blocking(move |db| db.get_tool_run(&tool_run_id))
            .await
    }

    /// Read only the producer status/result needed by a dependent scheduled
    /// ToolRun. This private projection is not used by UI/history commands.
    pub async fn get_tool_run_dependency(
        &self,
        tool_run_id: String,
    ) -> anyhow::Result<Option<ToolRunDependencyRow>> {
        self.db
            .run_blocking(move |db| db.get_tool_run_dependency(&tool_run_id))
            .await
    }

    /// Remove a persisted ToolRun of either kind.
    pub async fn delete_tool_run(&self, tool_run_id: String) -> anyhow::Result<bool> {
        self.db
            .run_blocking(move |db| db.delete_tool_run(&tool_run_id))
            .await
    }

    /// Atomically delete terminal history while retaining pending/running
    /// work and undelivered completion results.
    pub async fn clear_terminal_tool_runs(&self) -> anyhow::Result<Vec<String>> {
        self.db
            .run_blocking(|db| db.clear_terminal_tool_runs())
            .await
    }

    /// Move a malformed waiting scheduled row into terminal history.
    pub async fn quarantine_waiting_scheduled_tool_run(
        &self,
        tool_run_id: String,
        error_reason: String,
        finished_at: String,
    ) -> anyhow::Result<bool> {
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
        self.db
            .run_blocking(move |db| db.cancel_background_tool_run(&tool_run_id, &finished_at))
            .await
    }

    /// Commit a completed/failed background row and its completion outbox
    /// record atomically. The outbox is acknowledged by the caller only after
    /// durable transcript projection.
    #[allow(clippy::too_many_arguments)]
    pub async fn finish_background_tool_run_with_completion(
        &self,
        tool_run_id: String,
        status: ToolRunStatus,
        output: Option<String>,
        error: Option<String>,
        error_reason: Option<String>,
        log_path: Option<String>,
        exit_code: Option<i32>,
        finished_at: String,
        status_json: Value,
    ) -> anyhow::Result<bool> {
        let status_json = serde_json::to_string(&status_json)?;
        self.db
            .run_blocking(move |db| {
                db.finish_tool_run_with_completion(
                    &tool_run_id,
                    status,
                    output.as_deref(),
                    error.as_deref(),
                    error_reason.as_deref(),
                    log_path.as_deref(),
                    exit_code,
                    &finished_at,
                    &status_json,
                )
            })
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
        mode: String,
        session_id: Option<String>,
        tool_name: Option<String>,
        tool_args: Option<String>,
        prompt: Option<String>,
        watch_tool_run_id: Option<String>,
    ) -> anyhow::Result<()> {
        self.db
            .run_blocking(move |db| {
                db.save_scheduled_tool_run(
                    &tool_run_id,
                    &due_at,
                    &title,
                    &body,
                    &mode,
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
            .run_blocking(|db| db.list_pending_scheduled_tool_runs())
            .await
    }

    /// List scheduled rows that have not reached a terminal state, including
    /// triggers already started by another ToolRunService instance.
    pub async fn list_live_scheduled_tool_runs(&self) -> anyhow::Result<Vec<ScheduledToolRunRow>> {
        self.db
            .run_blocking(|db| db.list_live_scheduled_tool_runs())
            .await
    }

    /// Claim a waiting scheduled trigger with a waiting-to-running CAS.
    pub async fn start_scheduled_tool_run(
        &self,
        tool_run_id: String,
        started_at: String,
    ) -> anyhow::Result<bool> {
        self.db
            .run_blocking(move |db| db.start_scheduled_tool_run(&tool_run_id, &started_at))
            .await
    }

    /// Return an undelivered scheduled fire to its waiting state.
    pub async fn requeue_scheduled_tool_run(&self, tool_run_id: String) -> anyhow::Result<bool> {
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

    /// Claim execution for one pending scheduled confirmation. Repeating with
    /// the same request ID is idempotent, so callers can retry an approval
    /// after a grant write failure without reopening cancellation races.
    pub async fn claim_scheduled_tool_run_execution(
        &self,
        tool_run_id: String,
        request_id: String,
    ) -> anyhow::Result<bool> {
        self.db
            .run_blocking(move |db| {
                db.claim_scheduled_tool_run_execution(&tool_run_id, &request_id)
            })
            .await
    }

    /// Release an execution claim when a later retryable approval step fails.
    pub async fn release_scheduled_tool_run_execution_claim(
        &self,
        tool_run_id: String,
        request_id: String,
    ) -> anyhow::Result<bool> {
        self.db
            .run_blocking(move |db| {
                db.release_scheduled_tool_run_execution_claim(&tool_run_id, &request_id)
            })
            .await
    }

    /// Cancel a waiting or running scheduled ToolRun.
    pub async fn cancel_scheduled_tool_run(
        &self,
        tool_run_id: String,
        finished_at: String,
    ) -> anyhow::Result<bool> {
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
    use serde_json::json;

    fn store() -> (Arc<Database>, ToolRunStore) {
        let db = Arc::new(Database::open_in_memory().unwrap());
        let store = ToolRunStore::new(db.clone());
        (db, store)
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
                "started".into(),
                Some(source_step_id.clone()),
            )
            .await
            .unwrap();

        let row = store.get_tool_run(tool_run_id).await.unwrap().unwrap();
        assert_eq!(row.source_step_id.as_deref(), Some(source_step_id.as_str()));
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
                "started".into(),
            )
            .await
            .unwrap();

        assert!(
            store
                .finish_background_tool_run_with_completion(
                    tool_run_id.clone(),
                    ToolRunStatus::Completed,
                    Some("durable result".into()),
                    None,
                    None,
                    None,
                    Some(0),
                    "finished".into(),
                    json!({"tool_run_id":tool_run_id, "status":"completed", "output":"durable result"}),
                )
                .await
                .unwrap()
        );

        let claimed = store.claim_pending_completion().await.unwrap().unwrap();
        assert_eq!(claimed.tool_run_id, tool_run_id);
        assert_eq!(claimed.session_id.as_deref(), Some(session_id.as_str()));
        assert_eq!(claimed.status_json["output"], "durable result");
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
                "started".into(),
            )
            .await
            .unwrap();

        assert!(
            store
                .finish_background_tool_run_with_completion(
                    tool_run_id.clone(),
                    ToolRunStatus::Completed,
                    Some("winner".into()),
                    None,
                    None,
                    None,
                    Some(0),
                    "winner finish".into(),
                    json!({"tool_run_id":tool_run_id, "status":"completed", "output":"winner"}),
                )
                .await
                .unwrap()
        );
        assert!(
            !store
                .finish_background_tool_run_with_completion(
                    tool_run_id.clone(),
                    ToolRunStatus::Failed,
                    None,
                    Some("late".into()),
                    Some("late".into()),
                    None,
                    Some(1),
                    "late finish".into(),
                    json!({"tool_run_id":tool_run_id, "status":"failed", "error":"late"}),
                )
                .await
                .unwrap()
        );

        let row = store.get_tool_run(tool_run_id).await.unwrap().unwrap();
        assert_eq!(row.status, ToolRunStatus::Completed);
        assert_eq!(row.output.as_deref(), Some("winner"));
        let completion = store.claim_pending_completion().await.unwrap().unwrap();
        assert_eq!(completion.status, row.status);
        assert_eq!(completion.status_json["output"], "winner");
    }

    #[tokio::test]
    async fn scheduled_claim_terminal_and_quarantine_are_conditional() {
        let (_db, store) = store();
        let tool_run_id = new_id("toolrun");
        store
            .save_scheduled_tool_run(
                tool_run_id.clone(),
                "2026-09-25T10:00:00Z".into(),
                "Reminder".into(),
                "body".into(),
                "notify".into(),
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
                .start_scheduled_tool_run(tool_run_id.clone(), "started".into())
                .await
                .unwrap()
        );
        assert!(
            !store
                .start_scheduled_tool_run(tool_run_id.clone(), "late start".into())
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
                    "finished".into(),
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
                    "late finish".into(),
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
        store
            .save_scheduled_tool_run(
                invalid_tool_run_id.clone(),
                "invalid".into(),
                "Reminder".into(),
                "bad".into(),
                "notify".into(),
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
                .quarantine_waiting_scheduled_tool_run(
                    invalid_tool_run_id.clone(),
                    "invalid due_at".into(),
                    "quarantined".into(),
                )
                .await
                .unwrap()
        );
        assert!(
            !store
                .quarantine_waiting_scheduled_tool_run(
                    invalid_tool_run_id.clone(),
                    "late quarantine".into(),
                    "late".into(),
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
}
