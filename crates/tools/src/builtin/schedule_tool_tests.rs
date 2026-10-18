use std::sync::Arc;

use haven_memory::{Database, ScheduleStore};
use serde_json::json;

use super::{ScheduleOperation, ScheduleParams, ScheduleTool};
use crate::{ScheduleService, Tool};

fn params(operation: ScheduleOperation) -> ScheduleParams {
    ScheduleParams {
        operation,
        delay_secs: None,
        due_at: None,
        watch_tool_run_id: None,
        mode: None,
        title: None,
        body: None,
        tool_name: None,
        tool_args: None,
        prompt: None,
        repeat_interval_secs: None,
        schedule_id: None,
        session_id: None,
    }
}

async fn test_tool() -> (ScheduleTool, String) {
    let db = Arc::new(Database::open_in_memory().unwrap());
    let owner = db.create_session("Schedule tool owner").unwrap();
    let service = Arc::new(ScheduleService::new());
    service.set_store(Some(ScheduleStore::new(db))).await;
    (
        ScheduleTool {
            service,
            registry: None,
        },
        owner.session_id,
    )
}

#[tokio::test]
async fn set_list_and_cancel_use_one_durable_schedule_identity() {
    let (tool, session_id) = test_tool().await;
    let mut set = params(ScheduleOperation::Set);
    set.delay_secs = Some(60);
    set.body = Some("Check the result".into());
    set.tool_name = Some("notify".into());
    set.session_id = Some(session_id.clone());

    let result = tool
        .run(set, tokio_util::sync::CancellationToken::new())
        .await
        .unwrap();
    let schedule_id = result.output["schedule_id"].as_str().unwrap().to_string();
    assert!(schedule_id.starts_with("sched-"));
    assert!(
        result.output["schedule_run_id"]
            .as_str()
            .unwrap()
            .starts_with("schrun-")
    );

    let mut list = params(ScheduleOperation::List);
    list.session_id = Some(session_id.clone());
    let listed = tool
        .run(list, tokio_util::sync::CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(listed.output["schedules"].as_array().unwrap().len(), 1);
    assert_eq!(
        listed.output["schedules"][0]["schedule_id"],
        json!(schedule_id)
    );
    assert_eq!(
        listed.output["schedules"][0]["body"],
        json!("Check the result")
    );

    let mut cancel = params(ScheduleOperation::Cancel);
    cancel.schedule_id = Some(schedule_id.clone());
    cancel.session_id = Some(session_id.clone());
    let cancelled = tool
        .run(cancel, tokio_util::sync::CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(cancelled.output["schedule_id"], json!(schedule_id));

    assert!(
        tool.service
            .list_for_session(&session_id)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn set_requires_schedule_owner_and_rejects_mixed_triggers() {
    let (tool, _session_id) = test_tool().await;
    let mut missing_owner = params(ScheduleOperation::Set);
    missing_owner.delay_secs = Some(60);
    missing_owner.body = Some("Do a check".into());
    missing_owner.tool_name = Some("notify".into());
    assert!(
        tool.run(missing_owner, tokio_util::sync::CancellationToken::new())
            .await
            .is_err()
    );

    let mut mixed = params(ScheduleOperation::Set);
    mixed.delay_secs = Some(60);
    mixed.watch_tool_run_id = Some("toolrun-00000000000000000000000000000001".into());
    mixed.body = Some("Do a check".into());
    mixed.mode = Some(haven_common::schedule::ScheduleMode::Continue);
    mixed.prompt = Some("Continue".into());
    mixed.session_id = Some("ses-00000000000000000000000000000001".into());
    assert!(
        tool.run(mixed, tokio_util::sync::CancellationToken::new())
            .await
            .is_err()
    );
}

#[test]
fn schedule_schema_cancels_only_by_schedule_id() {
    let schema = ScheduleTool {
        service: Arc::new(ScheduleService::new()),
        registry: None,
    }
    .input_schema();
    let properties = schema["properties"].as_object().unwrap();
    assert!(properties.contains_key("schedule_id"));
    assert!(!properties.contains_key("tool_run_id"));
    let encoded = schema.to_string();
    assert!(encoded.contains("schedule plan id returned by schedule.set"));
}
