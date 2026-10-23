use super::support::make_test_agent;
use haven_common::types::ExecutionMode;

#[tokio::test]
async fn forked_session_inherits_parent_execution_workspace() {
    let (agent, executor) = make_test_agent();
    let parent = executor.create_session("fork workspace").await.unwrap();
    let workspace = tempfile::tempdir().unwrap();
    executor
        .set_session_execution_workspace(&parent.session_id, Some(workspace.path().to_path_buf()))
        .await
        .unwrap();
    executor
        .set_session_execution_mode(&parent.session_id, ExecutionMode::Unattended)
        .await
        .unwrap();
    let message = executor
        .session_store()
        .persist_ingress_user_seed(&parent.session_id, "hello", &[], false)
        .await
        .unwrap();
    let transcript = crate::types::TranscriptRecord::UserInject {
        step_number: 0,
        source: haven_common::types::InjectSource::FollowUp,
        text: "hello".into(),
        media_inputs: Vec::new(),
        message_id: Some(message.message_id.clone()),
    };
    agent
        .react_engine
        .seed_transcript_events(&parent.session_id, std::slice::from_ref(&transcript), 0)
        .await
        .unwrap();

    let child_session_id = agent
        .fork_session_from_message(&parent.session_id, &message.message_id)
        .await
        .unwrap();

    assert_eq!(
        executor
            .session_execution_workspace(&child_session_id)
            .await
            .unwrap()
            .as_deref(),
        Some(workspace.path())
    );
    assert_eq!(
        executor
            .session_execution_mode(&child_session_id)
            .await
            .unwrap(),
        ExecutionMode::Unattended
    );
}
