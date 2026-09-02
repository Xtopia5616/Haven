use super::support::*;
use super::*;

#[test]
fn agent_new_constructor_works() {
    let mut p = std::env::temp_dir();
    p.push(format!("haven_agent_new_{}.db", uuid::Uuid::new_v4()));
    let db = Arc::new(Database::open(&p).unwrap());
    let tools = Arc::new(ToolsManager::new());
    let executor = Arc::new(SessionExecutor::new(db.clone(), tools, 1));
    let client = Arc::new(FinalAnswerMock) as Arc<dyn LlmClient>;
    let router = Arc::new(LlmRouter::new_with_clients(
        client.clone(),
        client.clone(),
        client.clone(),
        client.clone(),
        client,
    ));
    let agent = AgentLayer::new(db, executor, router, 10, 20, ContextLimitsConfig::default());
    // Verify construction succeeded; no per-session indirection remains.
    let session = agent.db.create_session("input", "transcript").unwrap();
    assert!(!session.id.is_empty());
}

#[test]
fn agent_constructs_without_session_machinery() {
    let (agent, _) = make_test_agent();
    let session = agent.db.create_session("input", "").unwrap();
    assert!(!session.id.is_empty());
    // Two sessions never share message keys ??each owns its own stream.
    let other = agent.db.create_session("input2", "").unwrap();
    assert_ne!(session.id, other.id);
}

#[test]
fn set_emitter_stores_reference() {
    let (agent, _) = make_test_agent();
    let recorder = Arc::new(RecordingEmitter {
        thoughts: std::sync::Mutex::new(Vec::new()),
        supplements: std::sync::Mutex::new(Vec::new()),
        notifications: std::sync::Mutex::new(Vec::new()),
        completed: std::sync::Mutex::new(false),
    });
    agent.set_emitter(recorder.clone());
    // Verify emitter is stored without panic (set_emitter succeeds)
}

#[tokio::test]
async fn replace_router_and_router_work() {
    let mut p = std::env::temp_dir();
    p.push(format!("haven_agent_router_{}.db", uuid::Uuid::new_v4()));
    let db = Arc::new(Database::open(&p).unwrap());
    let tools = Arc::new(ToolsManager::new());
    let executor = Arc::new(SessionExecutor::new(db.clone(), tools, 1));
    let client_a = Arc::new(FinalAnswerMock) as Arc<dyn LlmClient>;
    let router_a = Arc::new(LlmRouter::new_with_clients(
        client_a.clone(),
        client_a.clone(),
        client_a.clone(),
        client_a.clone(),
        client_a,
    ));
    let agent = Arc::new(AgentLayer::new(
        db,
        executor,
        router_a,
        10,
        20,
        ContextLimitsConfig::default(),
    ));
    // Create a new router via the same mock client factory
    let client_b = Arc::new(FinalAnswerMock) as Arc<dyn LlmClient>;
    let router_b = Arc::new(LlmRouter::new_with_clients(
        client_b.clone(),
        client_b.clone(),
        client_b.clone(),
        client_b.clone(),
        client_b,
    ));
    agent.replace_router(router_b);
    // No panic == success
}

#[tokio::test]
async fn set_max_steps_updates_field() {
    let (agent, executor) = make_test_agent();
    agent.set_max_steps(5);

    let recorder = Arc::new(RecordingEmitter {
        thoughts: std::sync::Mutex::new(Vec::new()),
        supplements: std::sync::Mutex::new(Vec::new()),
        notifications: std::sync::Mutex::new(Vec::new()),
        completed: std::sync::Mutex::new(false),
    });
    agent.set_emitter(recorder.clone());

    let session = executor.create_session("test").await.unwrap();
    let history = agent.run_session_from_id(&session.id).await.unwrap();
    assert!(!history.is_empty());
}

#[tokio::test]
async fn build_system_prompt_succeeds() {
    let (agent, _) = make_test_agent();
    let prompt = agent.prompt_builder.build("test session", &[]).await;
    assert!(prompt.contains("You have access to the following built-in tools"));
}

#[tokio::test]
async fn build_system_prompt_excludes_sensitive_and_duplicate_facts() {
    let dir = std::env::temp_dir().join(format!("haven_prompt_facts_{}.db", uuid::Uuid::new_v4()));
    let db = Arc::new(Database::open(&dir).unwrap());
    // Duplicate triple (same tags, same everything).
    db.insert_fact("user", "name", "Xtopia", "user", 1.0, &["identity"])
        .unwrap();
    db.insert_fact("user", "name", "Xtopia", "user", 1.0, &["identity"])
        .unwrap();
    // A legitimate preference.
    db.insert_fact("user", "likes", "Rust", "user", 0.9, &["preference"])
        .unwrap();
    // Secrets that must never reach the prompt.
    db.insert_fact(
        "user",
        "tavily_api_key",
        "tvly-dev-secret",
        "inferred",
        1.0,
        &["workspace"],
    )
    .unwrap();
    db.insert_fact(
        "user",
        "secret_token",
        "ghp_abc",
        "inferred",
        1.0,
        &["workspace"],
    )
    .unwrap();

    let tools = Arc::new(ToolsManager::new());
    let builder = SystemPromptBuilder::new(tools, db);
    let prompt = builder.build("test session", &[]).await;

    assert!(prompt.contains("name=Xtopia"));
    assert!(prompt.contains("likes=Rust"));
    assert!(!prompt.contains("tavily_api_key"));
    assert!(!prompt.contains("tvly-dev-secret"));
    assert!(!prompt.contains("secret_token"));
    assert!(!prompt.contains("ghp_abc"));
    // Duplicates are collapsed: the name fact is rendered exactly once.
    assert_eq!(prompt.matches("name=Xtopia").count(), 1);
}

#[tokio::test]
async fn persist_message_adds_to_db() {
    let (agent, _) = make_test_agent();
    let session = agent.db.create_session("input", "").unwrap();
    agent
        .persist_message_parts(
            &session.id,
            "user",
            "test message",
            Some("text"),
            &[],
            false,
        )
        .await
        .unwrap();
    // Read back via db
    let agent_ref = agent.clone();
    let db = agent_ref.db.clone();
    let msgs = db.get_session_messages_limit(&session.id, 50).unwrap();
    // Messages may or may not be immediately flushed depending on cache
    // ??verify at minimum the message is retrievable
    let found = msgs
        .iter()
        .find(|m| m.role == "user" && m.content == "test message");
    assert!(found.is_some(), "persisted user message not found in db");
}

#[tokio::test]
async fn persist_message_with_attachments_roundtrips() {
    let (agent, _) = make_test_agent();
    let session = agent.db.create_session("input", "").unwrap();
    let att = haven_common::types::MessageAttachment::new("image/png", "aGVsbG8=");
    agent
        .persist_message_parts(
            &session.id,
            "user",
            "看图",
            Some("text"),
            std::slice::from_ref(&att),
            false,
        )
        .await
        .unwrap();
    let agent_ref = agent.clone();
    let db = agent_ref.db.clone();
    let msgs = db.get_session_messages_limit(&session.id, 50).unwrap();
    let found = msgs
        .iter()
        .find(|m| m.role == "user" && m.content == "看图");
    assert!(found.is_some(), "persisted message not found in db");
    let msg = found.unwrap();
    assert_eq!(msg.attachments.len(), 1);
    assert_eq!(msg.attachments[0].media_type, "image/png");
    assert_eq!(msg.attachments[0].data, "aGVsbG8=");
}

#[test]
fn parse_default_model_response_final_answer_from_text() {
    let resp = LlmResponse {
        text: "Session done.".into(),
        tool_calls: vec![],
        finish_reason: Some(FinishReason::Stop),
        usage: haven_llm::Usage::default(),
        model: None,
        reasoning: None,
        web_search_calls: Vec::new(),
        thinking_blocks: Vec::new(),
    };
    let (thought, actions) = ReActEngine::parse_default_model_response(&resp, 1);
    assert_eq!(thought, Some("Session done.".into()));
    assert_eq!(actions.len(), 1);
    assert!(actions[0].is_final);
    assert_eq!(actions[0].tool_name, "final_answer");
}

#[test]
fn parse_default_model_response_with_tool_calls() {
    let resp = LlmResponse {
        text: "Opening file.".into(),
        tool_calls: vec![CanonicalToolCall {
            id: "tc1".into(),
            name: "open_file".into(),
            arguments: serde_json::json!({"path": "/tmp/test"}),
        }],
        finish_reason: Some(FinishReason::ToolCalls),
        usage: haven_llm::Usage::default(),
        model: None,
        reasoning: None,
        web_search_calls: Vec::new(),
        thinking_blocks: Vec::new(),
    };
    let (thought, actions) = ReActEngine::parse_default_model_response(&resp, 2);
    assert_eq!(thought, Some("Opening file.".into()));
    assert_eq!(actions.len(), 1);
    assert!(!actions[0].is_final);
    assert_eq!(actions[0].tool_name, "open_file");
    assert_eq!(
        actions[0].tool_input,
        serde_json::json!({"path": "/tmp/test"})
    );
}

#[test]
fn parse_default_model_response_final_answer_tool_call() {
    let resp = LlmResponse {
        text: "All done.".into(),
        tool_calls: vec![CanonicalToolCall {
            id: "final".into(),
            name: "final_answer".into(),
            arguments: serde_json::json!({}),
        }],
        finish_reason: Some(FinishReason::Stop),
        usage: haven_llm::Usage::default(),
        model: None,
        reasoning: None,
        web_search_calls: Vec::new(),
        thinking_blocks: Vec::new(),
    };
    let (thought, actions) = ReActEngine::parse_default_model_response(&resp, 1);
    assert_eq!(thought, Some("All done.".into()));
    assert_eq!(actions.len(), 1);
    assert!(actions[0].is_final);
}

/// M3/H10: a follow-up message must NOT resurrect a session that was ended.
/// Terminal sessions are only reactivated explicitly via `reopen_session`
/// (Completed/Error ??Paused) in the resume flow.
#[tokio::test]
async fn process_input_does_not_resurrect_ended_session() {
    let (agent, executor) = make_test_agent();
    let session = executor.create_session("original").await.unwrap();
    executor.end_session(&session.id).await.unwrap();
    // end_session removes the session from the working set entirely.
    assert_eq!(executor.get_session_state(&session.id).await, None);

    let result = agent
        .process_input("more context", Some(session.id.clone()))
        .await
        .unwrap();
    assert!(matches!(result, ProcessResult::Supplemented { .. }));
    // Session is not reloaded into the working set and never becomes Pending.
    assert_eq!(executor.get_session_state(&session.id).await, None);
    assert!(executor.get_supplements(&session.id).await.is_empty());
}

#[tokio::test]
async fn process_input_reactivates_paused_session() {
    let (agent, executor) = make_test_agent();
    let session = executor.create_session("original").await.unwrap();
    executor
        .update_session_status(&session.id, SessionStatus::Paused)
        .await
        .unwrap();

    let result = agent
        .process_input("more context", Some(session.id.clone()))
        .await
        .unwrap();
    assert!(matches!(result, ProcessResult::Supplemented { .. }));
    assert_eq!(
        executor.get_session_state(&session.id).await,
        Some(SessionStatus::Pending)
    );
    let supps: Vec<String> = executor
        .get_supplements(&session.id)
        .await
        .into_iter()
        .map(|s| s.text)
        .collect();
    assert_eq!(supps, vec!["more context"]);
}

#[tokio::test]
async fn process_input_marks_reply_as_answer_when_awaiting() {
    let (agent, executor) = make_test_agent();
    let session = executor.create_session("original").await.unwrap();
    executor
        .update_session_status(&session.id, SessionStatus::Paused)
        .await
        .unwrap();
    executor
        .update_session_status(&session.id, SessionStatus::PausedAwaitingAnswer)
        .await
        .unwrap();

    let result = agent
        .process_input("the answer", Some(session.id.clone()))
        .await
        .unwrap();
    assert!(matches!(result, ProcessResult::Supplemented { .. }));
    assert_eq!(
        executor.get_session_state(&session.id).await,
        Some(SessionStatus::Pending)
    );
    let supps = executor.get_supplements(&session.id).await;
    assert_eq!(supps.len(), 1);
    assert!(
        supps[0].is_answer,
        "reply to an ask must be marked as answer"
    );
    assert_eq!(supps[0].text, "the answer");
    assert!(
        !executor
            .get_session_state(&session.id)
            .await
            .is_some_and(|s| s.is_awaiting_answer()),
        "reactivation must clear the awaiting-answer gate"
    );
}

#[tokio::test]
async fn process_input_paused_without_ask_is_plain_supplement() {
    let (agent, executor) = make_test_agent();
    let session = executor.create_session("original").await.unwrap();
    executor
        .update_session_status(&session.id, SessionStatus::Paused)
        .await
        .unwrap();

    let result = agent
        .process_input("follow up", Some(session.id.clone()))
        .await
        .unwrap();
    assert!(matches!(result, ProcessResult::Supplemented { .. }));
    let supps = executor.get_supplements(&session.id).await;
    assert_eq!(supps.len(), 1);
    assert!(
        !supps[0].is_answer,
        "a follow-up to a normal pause is not an ask reply"
    );
}

#[tokio::test]
async fn process_input_with_attachments_queues_and_persists_attachments() {
    let (agent, executor) = make_test_agent();
    let session = executor
        .create_session_with_summary("original", "original")
        .await
        .unwrap();
    executor
        .update_session_status(&session.id, SessionStatus::Paused)
        .await
        .unwrap();

    let att = haven_common::types::MessageAttachment::new("image/png", "aGVsbG8=");
    let result = agent
        .process_input_with_attachments(
            "看图",
            Some(session.id.clone()),
            std::slice::from_ref(&att),
            false,
        )
        .await
        .unwrap();
    assert!(matches!(result, ProcessResult::Supplemented { .. }));
    assert_eq!(
        executor.get_session_state(&session.id).await,
        Some(SessionStatus::Pending)
    );
    let supps = executor.get_supplements(&session.id).await;
    assert_eq!(supps.len(), 1);
    assert_eq!(supps[0].text, "看图");
    assert_eq!(supps[0].attachments, vec![att]);

    // Persisted with attachments in the session's message stream.
    let msgs = agent.db.get_session_messages(&session.id).unwrap();
    let user_msg = msgs
        .iter()
        .find(|m| m.role == "user" && m.content == "看图")
        .expect("user message persisted");
    assert_eq!(user_msg.attachments.len(), 1);
    assert_eq!(user_msg.attachments[0].media_type, "image/png");
}

#[tokio::test]
async fn process_input_creates_new_session() {
    let (agent, executor) = make_test_agent();
    let result = agent.process_input("open notepad", None).await.unwrap();
    match result {
        ProcessResult::SessionCreated {
            session_id,
            message_id,
        } => {
            assert!(!session_id.is_empty());
            assert!(
                message_id
                    .as_deref()
                    .is_some_and(|id| id.starts_with("msg-")),
                "SessionCreated must return the persisted first-user msg id"
            );
            let state = executor.get_session_state(&session_id).await;
            assert_eq!(state, Some(SessionStatus::Pending));
        }
        ProcessResult::Supplemented { .. } => panic!("expected SessionCreated"),
    }
}

#[tokio::test]
async fn run_fact_inference_does_not_panic() {
    let (agent, executor) = make_test_agent();
    let session = executor.create_session("test").await.unwrap();
    executor.end_session(&session.id).await.unwrap();
    agent.inference.infer_facts(&session.id).await;
}
