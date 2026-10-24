use std::sync::Arc;

use chrono::{SecondsFormat, TimeZone, Utc};
use haven_common::lifecycle::ScheduleRunStatus;
use haven_common::schedule::ScheduleMode;

use super::{SchedulePlanInput, ScheduleStore, ScheduleType};
use crate::Database;

fn timestamp(seconds: i64) -> String {
    Utc.timestamp_opt(seconds, 0)
        .single()
        .unwrap()
        .to_rfc3339_opts(SecondsFormat::Millis, true)
}

fn tool_plan(owner_session_id: String, due_at: String) -> SchedulePlanInput {
    SchedulePlanInput {
        owner_session_id,
        schedule_type: ScheduleType::Once,
        title: "Run tool later".into(),
        body: "Run a bounded action".into(),
        mode: ScheduleMode::Tool,
        due_at: Some(due_at),
        watch_tool_run_id: None,
        tool_name: Some("notify".into()),
        tool_args: Some(serde_json::json!({"title":"done"})),
        prompt: None,
        interval_seconds: None,
    }
}

#[tokio::test]
async fn one_off_schedule_claims_once_and_finishes_independently() {
    let db = Arc::new(Database::open_in_memory().unwrap());
    let owner = db.create_session("schedule owner").unwrap();
    let store = ScheduleStore::new(db);
    let plan = store
        .create(tool_plan(owner.session_id, timestamp(1_800_000_000)))
        .await
        .unwrap();

    assert!(plan.schedule_id.starts_with("sched-"));
    let first = store.claim_due(timestamp(1_800_000_001), 16).await.unwrap();
    assert_eq!(first.len(), 1);
    assert!(first[0].run.schedule_run_id.starts_with("schrun-"));
    assert_eq!(first[0].run.status, ScheduleRunStatus::Running);
    assert_eq!(first[0].plan.schedule_id, plan.schedule_id);
    assert!(
        store
            .claim_due(timestamp(1_800_000_002), 16)
            .await
            .unwrap()
            .is_empty()
    );

    assert!(
        store
            .claim_execution(
                first[0].run.schedule_run_id.clone(),
                first[0].run.schedule_run_id.clone(),
            )
            .await
            .unwrap()
    );
    assert!(
        store
            .finish(
                first[0].run.schedule_run_id.clone(),
                ScheduleRunStatus::Completed,
                Some("tool completed".into()),
                None,
                timestamp(1_800_000_003),
            )
            .await
            .unwrap()
    );
    assert!(store.list_enabled().await.unwrap().is_empty());
}

#[tokio::test]
async fn schedule_rejects_due_dates_more_than_ten_years_ahead() {
    let db = Arc::new(Database::open_in_memory().unwrap());
    let owner = db.create_session("schedule horizon owner").unwrap();
    let store = ScheduleStore::new(db);
    let too_far = Utc::now() + chrono::Duration::days(10 * 365 + 1);
    let result = store
        .create(tool_plan(
            owner.session_id,
            too_far.to_rfc3339_opts(SecondsFormat::Millis, true),
        ))
        .await;

    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("cannot be more than 10 years")
    );
}

#[tokio::test]
async fn recurring_schedule_advances_from_anchor_without_overlapping_runs() {
    let db = Arc::new(Database::open_in_memory().unwrap());
    let owner = db.create_session("recurring owner").unwrap();
    let store = ScheduleStore::new(db);
    let plan = store
        .create(SchedulePlanInput {
            owner_session_id: owner.session_id,
            schedule_type: ScheduleType::RecurringSession,
            title: "Daily check".into(),
            body: "Create a new session".into(),
            mode: ScheduleMode::NewSession,
            due_at: Some(timestamp(1_800_000_000)),
            watch_tool_run_id: None,
            tool_name: None,
            tool_args: None,
            prompt: Some("Review today's notes".into()),
            interval_seconds: Some(3_600),
        })
        .await
        .unwrap();

    let fired = store.claim_due(timestamp(1_800_000_001), 16).await.unwrap();
    assert_eq!(fired.len(), 1);
    assert_eq!(fired[0].plan.schedule_id, plan.schedule_id);
    assert!(
        store
            .claim_due(timestamp(1_800_000_002), 16)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        store
            .finish(
                fired[0].run.schedule_run_id.clone(),
                ScheduleRunStatus::Failed,
                None,
                Some("interrupted test".into()),
                timestamp(1_800_000_100),
            )
            .await
            .unwrap()
    );

    let enabled = store.list_enabled().await.unwrap();
    assert_eq!(enabled.len(), 1);
    assert_eq!(
        enabled[0].next_due_at.as_deref(),
        Some(timestamp(1_800_003_600).as_str())
    );
    let next = store.claim_due(timestamp(1_800_003_601), 16).await.unwrap();
    assert_eq!(next.len(), 1);
    assert_ne!(next[0].run.schedule_run_id, fired[0].run.schedule_run_id);
}

#[tokio::test]
async fn cancellation_cannot_take_a_claimed_schedule_execution() {
    let db = Arc::new(Database::open_in_memory().unwrap());
    let owner = db.create_session("cancel owner").unwrap();
    let store = ScheduleStore::new(db);
    let plan = store
        .create(tool_plan(
            owner.session_id.clone(),
            timestamp(1_800_000_000),
        ))
        .await
        .unwrap();
    let fired = store.claim_due(timestamp(1_800_000_001), 16).await.unwrap();
    let run_id = fired[0].run.schedule_run_id.clone();

    assert!(
        store
            .claim_execution(run_id.clone(), run_id.clone())
            .await
            .unwrap()
    );
    assert!(
        !store
            .cancel(plan.schedule_id, owner.session_id, timestamp(1_800_000_002))
            .await
            .unwrap()
    );
    assert_eq!(
        store.list_live_runs().await.unwrap()[0].status,
        ScheduleRunStatus::Running
    );
}

#[tokio::test]
async fn restart_requeues_unclaimed_occurrences_with_their_stable_identity() {
    let db = Arc::new(Database::open_in_memory().unwrap());
    let owner = db.create_session("unclaimed schedule restart").unwrap();
    let store = ScheduleStore::new(db);
    let plan = store
        .create(tool_plan(owner.session_id, timestamp(1_800_000_000)))
        .await
        .unwrap();
    let first = store
        .claim_due(timestamp(1_800_000_001), 1)
        .await
        .unwrap()
        .pop()
        .unwrap();

    let recovered = store
        .recover_interrupted(timestamp(1_800_000_002))
        .await
        .unwrap();
    assert_eq!(recovered.requeued_before_execution, 1);
    assert_eq!(recovered.failed_with_unknown_outcome, 0);
    let waiting = store
        .get_run(first.run.schedule_run_id.clone())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(waiting.status, ScheduleRunStatus::Waiting);
    assert!(
        waiting
            .error_reason
            .as_deref()
            .unwrap()
            .contains("safe to retry")
    );

    let retried = store
        .claim_due(timestamp(1_800_000_003), 1)
        .await
        .unwrap()
        .pop()
        .unwrap();
    assert_eq!(retried.run.schedule_run_id, first.run.schedule_run_id);
    assert_eq!(retried.plan.schedule_id, plan.schedule_id);
}

#[tokio::test]
async fn restart_marks_claimed_occurrences_unknown_and_never_replays_them() {
    let db = Arc::new(Database::open_in_memory().unwrap());
    let owner = db.create_session("claimed schedule restart").unwrap();
    let store = ScheduleStore::new(db);
    let plan = store
        .create(tool_plan(owner.session_id, timestamp(1_800_000_000)))
        .await
        .unwrap();
    let fired = store
        .claim_due(timestamp(1_800_000_001), 1)
        .await
        .unwrap()
        .pop()
        .unwrap();
    assert!(
        store
            .claim_execution(
                fired.run.schedule_run_id.clone(),
                fired.run.schedule_run_id.clone(),
            )
            .await
            .unwrap()
    );

    let recovered = store
        .recover_interrupted(timestamp(1_800_000_002))
        .await
        .unwrap();
    assert_eq!(recovered.requeued_before_execution, 0);
    assert_eq!(recovered.failed_with_unknown_outcome, 1);
    let failed = store
        .get_run(fired.run.schedule_run_id.clone())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(failed.status, ScheduleRunStatus::Failed);
    let reason = failed.error_reason.unwrap();
    assert!(reason.contains("outcome is unknown"));
    assert!(reason.contains("automatic retry suppressed"));
    assert!(store.list_enabled().await.unwrap().is_empty());
    assert!(
        store
            .claim_due(timestamp(1_800_000_003), 1)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(plan.schedule_id, fired.plan.schedule_id);
}

#[tokio::test]
async fn restart_requeues_the_same_recurring_occurrence_before_execution() {
    let db = Arc::new(Database::open_in_memory().unwrap());
    let owner = db.create_session("recurring restart owner").unwrap();
    let store = ScheduleStore::new(db);
    let plan = store
        .create(SchedulePlanInput {
            owner_session_id: owner.session_id,
            schedule_type: ScheduleType::RecurringSession,
            title: "Recurring restart test".into(),
            body: "Create one session".into(),
            mode: ScheduleMode::NewSession,
            due_at: Some(timestamp(1_800_000_000)),
            watch_tool_run_id: None,
            tool_name: None,
            tool_args: None,
            prompt: Some("Review the notes".into()),
            interval_seconds: Some(3_600),
        })
        .await
        .unwrap();
    let first = store
        .claim_due(timestamp(1_800_000_001), 1)
        .await
        .unwrap()
        .pop()
        .unwrap();

    let recovered = store
        .recover_interrupted(timestamp(1_800_000_002))
        .await
        .unwrap();
    assert_eq!(recovered.requeued_before_execution, 1);
    let retried = store
        .claim_due(timestamp(1_800_000_003), 1)
        .await
        .unwrap()
        .pop()
        .unwrap();
    assert_eq!(retried.run.schedule_run_id, first.run.schedule_run_id);
    assert_eq!(retried.plan.schedule_id, plan.schedule_id);
}
