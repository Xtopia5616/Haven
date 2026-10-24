//! Typed asynchronous persistence boundary for Schedule plans and occurrences.
//!
//! Schedule owns trigger plans and durable occurrences. It does not persist
//! trigger rows in ToolRun storage; a ScheduleRun only becomes a ToolCall (or
//! a session input) after the trigger has been claimed.

#[path = "schedule_store_db.rs"]
mod schedule_store_db;
#[path = "schedule_store_types.rs"]
mod schedule_store_types;

pub use schedule_store_types::{
    SchedulePlanInput, SchedulePlanRow, ScheduleRecoverySummary, ScheduleRunFired, ScheduleRunRow,
    ScheduleType, WatchedToolRunResult,
};

use std::sync::Arc;

use haven_common::lifecycle::ScheduleRunStatus;

use crate::Database;
#[derive(Clone)]
pub struct ScheduleStore {
    db: Arc<Database>,
}

impl ScheduleStore {
    #[cfg(feature = "test-support")]
    pub fn new(db: Arc<Database>) -> Self {
        Self { db }
    }

    #[cfg(not(feature = "test-support"))]
    pub(crate) fn new(db: Arc<Database>) -> Self {
        Self { db }
    }

    pub async fn create(&self, input: SchedulePlanInput) -> anyhow::Result<SchedulePlanRow> {
        self.db
            .run_blocking(move |db| db.create_schedule(input))
            .await
    }

    pub async fn list_for_session(
        &self,
        owner_session_id: String,
    ) -> anyhow::Result<Vec<SchedulePlanRow>> {
        self.db
            .run_blocking(move |db| db.list_schedules_for_session(&owner_session_id))
            .await
    }

    pub async fn list_enabled(&self) -> anyhow::Result<Vec<SchedulePlanRow>> {
        self.db.run_blocking(Database::list_enabled_schedules).await
    }

    pub async fn list_live_runs(&self) -> anyhow::Result<Vec<ScheduleRunRow>> {
        self.db
            .run_blocking(Database::list_live_schedule_runs)
            .await
    }

    pub async fn get_run(&self, schedule_run_id: String) -> anyhow::Result<Option<ScheduleRunRow>> {
        self.db
            .run_blocking(move |db| db.get_schedule_run(&schedule_run_id))
            .await
    }

    /// Atomically claim due one-off and recurring occurrences. Each returned
    /// row is already `running` in durable storage, so consumers can safely
    /// proceed to authorization and execution-claim arbitration.
    pub async fn claim_due(
        &self,
        now: String,
        limit: usize,
    ) -> anyhow::Result<Vec<ScheduleRunFired>> {
        self.db
            .run_blocking(move |db| db.claim_due_schedule_runs(&now, limit))
            .await
    }

    pub async fn cancel(
        &self,
        schedule_id: String,
        owner_session_id: String,
        finished_at: String,
    ) -> anyhow::Result<bool> {
        self.db
            .run_blocking(move |db| {
                db.cancel_schedule(&schedule_id, &owner_session_id, &finished_at)
            })
            .await
    }

    pub async fn claim_execution(
        &self,
        schedule_run_id: String,
        claim_id: String,
    ) -> anyhow::Result<bool> {
        self.db
            .run_blocking(move |db| db.claim_schedule_run_execution(&schedule_run_id, &claim_id))
            .await
    }

    pub async fn release_execution(
        &self,
        schedule_run_id: String,
        claim_id: String,
    ) -> anyhow::Result<bool> {
        self.db
            .run_blocking(move |db| db.release_schedule_run_execution(&schedule_run_id, &claim_id))
            .await
    }

    pub async fn finish(
        &self,
        schedule_run_id: String,
        status: ScheduleRunStatus,
        result_summary: Option<String>,
        error_reason: Option<String>,
        finished_at: String,
    ) -> anyhow::Result<bool> {
        self.db
            .run_blocking(move |db| {
                db.finish_schedule_run(
                    &schedule_run_id,
                    status,
                    result_summary.as_deref(),
                    error_reason.as_deref(),
                    &finished_at,
                )
            })
            .await
    }

    /// Recover prior-process occurrences. Rows without an execution claim are
    /// safely requeued with the same ID; claimed rows fail closed as unknown.
    pub async fn recover_interrupted(
        &self,
        finished_at: String,
    ) -> anyhow::Result<ScheduleRecoverySummary> {
        self.db
            .run_blocking(move |db| db.recover_interrupted_schedule_runs(&finished_at))
            .await
    }
}

#[cfg(test)]
#[path = "schedule_store_tests.rs"]
mod tests;
