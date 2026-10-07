use super::support::*;
use super::*;
use haven_memory::ToolRunStore;

#[derive(Default)]
struct SessionUpdateCapture(std::sync::Mutex<Vec<(String, SessionStatus)>>);

#[async_trait::async_trait]
impl AgentEventEmitter for SessionUpdateCapture {
    async fn emit(&self, event: AgentEvent) {
        if let AgentEvent::SessionUpdated {
            session_id, status, ..
        } = event
        {
            self.0.lock().unwrap().push((session_id, status));
        }
    }
}

fn make_in_memory_agent() -> (Arc<AgentLayer>, Arc<SessionSupervisor>, Arc<Database>) {
    let db = Arc::new(Database::open_in_memory().unwrap());
    let (agent, executor) = make_test_agent_with_db(
        db.clone(),
        Arc::new(FinalAnswerMock),
        Arc::new(ToolsFacade::new()),
        ContextLimitsConfig::default(),
    );
    (agent, executor, db)
}

async fn assert_dispatcher_waits_for_memory_runtime(recover_pending: bool) {
    let (agent, memory_startup, executor) = make_test_agent_with_startup();
    let session = executor
        .create_session("startup readiness barrier")
        .await
        .unwrap();
    agent
        .db
        .conn()
        .execute_batch(
            "CREATE TRIGGER block_memory_runtime_cursor BEFORE INSERT ON kv_store
             WHEN NEW.key GLOB 'memory_event_cursor.*'
             BEGIN SELECT RAISE(ABORT, 'test startup barrier'); END;",
        )
        .unwrap();

    let cancellation = CancellationToken::new();
    let prepare_cancellation = cancellation.clone();
    let prepare_startup = memory_startup.clone();
    let prepare =
        tokio::spawn(async move { prepare_startup.prepare_start(&prepare_cancellation).await });

    // prepare_start retries the injected persistence failure. The session is
    // already queued, so no typed readiness handoff exists until it succeeds.
    tokio::time::sleep(std::time::Duration::from_millis(350)).await;
    assert_eq!(
        executor.get_session_status(&session.id).await,
        Some(SessionStatus::Pending),
        "dispatcher ran before memory runtime preparation completed"
    );
    assert_eq!(
        agent
            .db
            .get_kv(&format!("memory_event_cursor.{}", session.id))
            .unwrap(),
        None,
        "injected failure unexpectedly installed the startup cursor"
    );

    agent
        .db
        .clone()
        .run_blocking(|db| {
            db.conn()
                .execute_batch("DROP TRIGGER block_memory_runtime_cursor")?;
            Ok(())
        })
        .await
        .unwrap();

    let prepared = tokio::time::timeout(std::time::Duration::from_secs(3), prepare)
        .await
        .expect("memory preparation did not recover after the injected failure")
        .unwrap()
        .unwrap();

    assert_eq!(
        executor.get_session_status(&session.id).await,
        Some(SessionStatus::Pending),
        "dispatcher opened before the app registered the prepared live consumer"
    );

    let live_consumer_handoff =
        memory_startup.prepare_live_consumer(prepared, cancellation.clone());
    let mut live_task = None;
    let readiness = live_consumer_handoff
        .register_consumer_with(|live_future| {
            live_task = Some(tokio::spawn(live_future));
            Some(())
        })
        .expect("test registry accepted the prepared live consumer");
    let live_task = live_task.expect("registry received the live future");
    agent.clone().start_after_memory_ready(
        readiness,
        if recover_pending {
            PendingSessionRecovery::RecoverImmediately
        } else {
            PendingSessionRecovery::DeferUntilCatalogReady
        },
        cancellation.clone(),
    );

    let status = tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            let status = executor.get_session_status(&session.id).await;
            if status != Some(SessionStatus::Pending) {
                break status;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("dispatcher did not claim the session after memory runtime became ready");
    assert_ne!(
        status,
        Some(SessionStatus::Pending),
        "dispatcher left the session pending after memory runtime became ready"
    );

    cancellation.cancel();
    tokio::time::timeout(std::time::Duration::from_secs(1), live_task)
        .await
        .expect("prepared live memory consumer did not stop after cancellation")
        .unwrap();
}

#[tokio::test]
async fn both_dispatcher_start_modes_wait_for_memory_runtime_readiness() {
    assert_dispatcher_waits_for_memory_runtime(true).await;
    assert_dispatcher_waits_for_memory_runtime(false).await;
}

#[tokio::test]
async fn cancelling_memory_preparation_does_not_open_dispatcher() {
    let (agent, memory_startup, executor) = make_test_agent_with_startup();
    let session = executor
        .create_session("cancelled memory startup")
        .await
        .unwrap();
    agent
        .db
        .conn()
        .execute_batch(
            "CREATE TRIGGER block_memory_runtime_cursor BEFORE INSERT ON kv_store
             WHEN NEW.key GLOB 'memory_event_cursor.*'
             BEGIN SELECT RAISE(ABORT, 'test startup cancellation'); END;",
        )
        .unwrap();

    let cancellation = CancellationToken::new();
    let prepare_cancellation = cancellation.clone();
    let prepare =
        tokio::spawn(async move { memory_startup.prepare_start(&prepare_cancellation).await });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    cancellation.cancel();

    let result = tokio::time::timeout(std::time::Duration::from_secs(1), prepare)
        .await
        .expect("cancelled memory preparation did not exit")
        .unwrap();
    let error = match result {
        Ok(_) => panic!("cancelled memory preparation unexpectedly succeeded"),
        Err(error) => error,
    };
    assert!(error.to_string().contains("cancelled"));
    assert_eq!(
        executor.get_session_status(&session.id).await,
        Some(SessionStatus::Pending),
        "dispatcher must remain closed after cancelled memory preparation"
    );
}

#[test]
fn agent_build_constructor_works() {
    let mut p = std::env::temp_dir();
    p.push(format!("haven_agent_build_{}.db", uuid::Uuid::new_v4()));
    let db = Arc::new(Database::open(&p).unwrap());
    let tools = Arc::new(ToolsFacade::new());
    let executor = Arc::new(SessionSupervisor::new_for_test(
        db.clone(),
        tools.clone(),
        1,
    ));
    let client = Arc::new(FinalAnswerMock) as Arc<dyn LlmClient>;
    let router = Arc::new(LlmRouter::new_with_clients(
        client.clone(),
        client.clone(),
        client.clone(),
        client,
    ));
    let context_limits = ContextLimitsConfig::default();
    let memory_service = memory_service_for_test(db.clone(), router.clone(), &context_limits);
    let agent = AgentLayer::build(
        memory_service,
        executor,
        crate::AgentToolPorts::from_tools_facade(tools),
        router,
        10,
        20,
        context_limits,
    )
    .agent;
    // Verify construction succeeded; no per-session indirection remains.
    assert!(agent.db.list_facts_by_subject("user").unwrap().is_empty());
    let session = agent.db.create_session("input").unwrap();
    assert!(!session.id.is_empty());
}

#[test]
fn agent_constructs_without_session_machinery() {
    let (agent, _) = make_test_agent();
    let session = agent.db.create_session("input").unwrap();
    assert!(!session.id.is_empty());
    // Two sessions never share message keys ??each owns its own stream.
    let other = agent.db.create_session("input2").unwrap();
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
    let tools = Arc::new(ToolsFacade::new());
    let executor = Arc::new(SessionSupervisor::new_for_test(
        db.clone(),
        tools.clone(),
        1,
    ));
    let client_a = Arc::new(FinalAnswerMock) as Arc<dyn LlmClient>;
    let router_a = Arc::new(LlmRouter::new_with_clients(
        client_a.clone(),
        client_a.clone(),
        client_a.clone(),
        client_a,
    ));
    let context_limits = ContextLimitsConfig::default();
    let memory_service = memory_service_for_test(db.clone(), router_a.clone(), &context_limits);
    let agent = Arc::new(
        AgentLayer::build(
            memory_service,
            executor,
            crate::AgentToolPorts::from_tools_facade(tools),
            router_a,
            10,
            20,
            context_limits,
        )
        .agent,
    );
    // Create a new router via the same mock client factory
    let client_b = Arc::new(FinalAnswerMock) as Arc<dyn LlmClient>;
    let router_b = Arc::new(LlmRouter::new_with_clients(
        client_b.clone(),
        client_b.clone(),
        client_b.clone(),
        client_b,
    ));
    agent.replace_router(router_b).unwrap();
    // No panic == success
}

#[tokio::test]
async fn set_max_steps_per_run_updates_field() {
    let (agent, executor) = make_test_agent();
    agent.set_max_steps_per_run(5).unwrap();

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
    assert!(prompt.contains("Available capabilities:"));
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

    let tools = Arc::new(ToolsFacade::new());
    let builder =
        SystemPromptBuilder::with_memory_service(tools, Arc::new(MemoryService::new(db, None, 64)));
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
    let session = agent.db.create_session("input").unwrap();
    agent
        .persist_message_parts(
            &session.id,
            haven_common::types::CanonicalRole::User,
            "test message",
            Some(haven_common::types::TranscriptMessageKind::Text),
            &[],
            false,
        )
        .await
        .unwrap();
    // Read back via db
    let agent_ref = agent.clone();
    let db = agent_ref.db.clone();
    let msgs = db.list_recent_session_messages(&session.id, 50).unwrap();
    // Messages may or may not be immediately flushed depending on cache
    // ??verify at minimum the message is retrievable
    let found = msgs.iter().find(|m| {
        m.role == haven_common::types::CanonicalRole::User && m.content == "test message"
    });
    assert!(found.is_some(), "persisted user message not found in db");
}

#[tokio::test]
async fn terminal_tool_run_result_projection_is_idempotent() {
    let (agent, executor) = make_test_agent();
    let session = executor
        .create_session("terminal ToolRun result")
        .await
        .unwrap();
    executor
        .update_session_status(&session.id, SessionStatus::Completed)
        .await
        .unwrap();

    let tool_run_result_id = "toolrun-terminal-dedup";
    let message_id = crate::react::tool_run_result_message_id(tool_run_result_id);
    let content =
        "[Background tool run result]\ntool_run_id: toolrun-terminal\nstatus: completed\n\nresult";
    let first = agent
        .executor
        .session_store()
        .persist_terminal_tool_run_result(&session.id, content, &message_id)
        .await
        .unwrap();
    let second = agent
        .executor
        .session_store()
        .persist_terminal_tool_run_result(&session.id, content, &message_id)
        .await
        .unwrap();

    assert_eq!(first.id, message_id);
    assert_eq!(second.id, message_id);
    assert_eq!(
        agent
            .db
            .list_session_messages(&session.id)
            .unwrap()
            .iter()
            .filter(|message| message.id == message_id)
            .count(),
        1,
        "terminal completion redelivery must reuse the existing history row"
    );
}

#[tokio::test]
async fn queued_tool_run_result_is_reconciled_after_session_becomes_terminal() {
    let (agent, memory_startup, executor) = make_test_agent_with_startup();
    let tool_run_service = executor.tool_run_service();
    tool_run_service
        .set_tool_run_store(Some(ToolRunStore::new(agent.db.clone())))
        .await;
    let session = executor
        .create_session("terminal ToolRun race")
        .await
        .unwrap();
    executor
        .update_session_status(&session.id, SessionStatus::Paused)
        .await
        .unwrap();

    let tool_run_id = "toolrun-terminal-race";
    agent
        .db
        .save_tool_run(tool_run_id, Some(&session.id), "echo race", "started")
        .unwrap();
    agent
        .db
        .finish_tool_run(
            tool_run_id,
            haven_common::ToolRunStatus::Completed,
            Some("race output"),
            None,
            None,
            None,
            Some(0),
            "finished",
        )
        .unwrap();

    // Model the transient consumer's successful queue admission, followed by
    // the session terminal cleanup that clears the actor queue before a turn
    // can project it. The durable outbox must remain the recovery authority.
    executor
        .add_tool_run_completion_with_id(
            &session.id,
            tool_run_id.to_string(),
            "[Background tool run result]\ntool_run_id: toolrun-terminal-race\nstatus: completed\n\nrace output",
        )
        .await
        .unwrap();
    executor
        .update_session_status(&session.id, SessionStatus::Completed)
        .await
        .unwrap();

    let cancellation = tokio_util::sync::CancellationToken::new();
    let prepared = memory_startup.prepare_start(&cancellation).await.unwrap();
    let live_consumer_handoff =
        memory_startup.prepare_live_consumer(prepared, cancellation.clone());
    let mut live_task = None;
    let readiness = live_consumer_handoff
        .register_consumer_with(|live_future| {
            live_task = Some(tokio::spawn(live_future));
            Some(())
        })
        .expect("test registry accepted the prepared live consumer");
    let live_task = live_task.expect("registry received the live future");
    agent.clone().start_after_memory_ready(
        readiness,
        PendingSessionRecovery::DeferUntilCatalogReady,
        cancellation.clone(),
    );
    let message_id = crate::react::tool_run_result_message_id(tool_run_id);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    loop {
        let has_message = agent
            .db
            .list_session_messages(&session.id)
            .unwrap()
            .iter()
            .any(|message| message.id == message_id);
        let pending: i64 = agent
            .db
            .conn()
            .query_row(
                "SELECT COUNT(*) FROM tool_run_completion_outbox
                 WHERE tool_run_id = ?1 AND delivered_at IS NULL",
                [tool_run_id],
                |row| row.get(0),
            )
            .unwrap();
        if has_message && pending == 0 {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "outbox result was not projected and acknowledged"
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    cancellation.cancel();
    tokio::time::timeout(std::time::Duration::from_secs(1), live_task)
        .await
        .expect("memory live task did not stop after cancellation")
        .unwrap();
}

#[tokio::test]
async fn unowned_terminal_completion_is_acknowledged_for_history_cleanup() {
    let (agent, memory_startup, executor) = make_test_agent_with_startup();
    let tool_run_service = executor.tool_run_service();
    tool_run_service
        .set_tool_run_store(Some(ToolRunStore::new(agent.db.clone())))
        .await;

    let tool_run_id = "toolrun-unowned-terminal";
    agent
        .db
        .save_tool_run(tool_run_id, None, "echo unowned", "started")
        .unwrap();
    agent
        .db
        .finish_tool_run(
            tool_run_id,
            haven_common::ToolRunStatus::Completed,
            Some("unowned output"),
            None,
            None,
            None,
            Some(0),
            "finished",
        )
        .unwrap();

    let cancellation = tokio_util::sync::CancellationToken::new();
    let prepared = memory_startup.prepare_start(&cancellation).await.unwrap();
    let live_consumer_handoff =
        memory_startup.prepare_live_consumer(prepared, cancellation.clone());
    let mut live_task = None;
    let readiness = live_consumer_handoff
        .register_consumer_with(|live_future| {
            live_task = Some(tokio::spawn(live_future));
            Some(())
        })
        .expect("test registry accepted the prepared live consumer");
    let live_task = live_task.expect("registry received the live future");
    agent.clone().start_after_memory_ready(
        readiness,
        PendingSessionRecovery::DeferUntilCatalogReady,
        cancellation.clone(),
    );

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    loop {
        let delivered: i64 = agent
            .db
            .conn()
            .query_row(
                "SELECT COUNT(*) FROM tool_run_completion_outbox
                 WHERE tool_run_id = ?1 AND delivered_at IS NOT NULL",
                [tool_run_id],
                |row| row.get(0),
            )
            .unwrap();
        if delivered == 1 {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "unowned completion was not acknowledged"
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }

    assert!(tool_run_service.delete_terminal(tool_run_id).await.unwrap());
    assert!(agent.db.get_tool_run(tool_run_id).unwrap().is_none());

    cancellation.cancel();
    tokio::time::timeout(std::time::Duration::from_secs(1), live_task)
        .await
        .expect("memory live task did not stop after cancellation")
        .unwrap();
}

#[tokio::test]
async fn persist_message_with_attachments_roundtrips() {
    let (agent, _) = make_test_agent();
    let session = agent.db.create_session("input").unwrap();
    let att = haven_common::types::MessageAttachment::new("image/png", "aGVsbG8=");
    agent
        .persist_message_parts(
            &session.id,
            haven_common::types::CanonicalRole::User,
            "看图",
            Some(haven_common::types::TranscriptMessageKind::Text),
            std::slice::from_ref(&att),
            false,
        )
        .await
        .unwrap();
    let agent_ref = agent.clone();
    let db = agent_ref.db.clone();
    let msgs = db.list_recent_session_messages(&session.id, 50).unwrap();
    let found = msgs
        .iter()
        .find(|m| m.role == haven_common::types::CanonicalRole::User && m.content == "看图");
    assert!(found.is_some(), "persisted message not found in db");
    let msg = found.unwrap();
    assert_eq!(msg.attachments.len(), 1);
    assert_eq!(msg.attachments[0].media_type, "image/png");
    assert!(
        msg.attachments[0].data.is_empty(),
        "the legacy DB projection must not retain inline bytes"
    );
    assert_eq!(msg.media_inputs.len(), 1);
    assert!(matches!(
        msg.media_inputs[0].representations[0].payload,
        haven_common::media::MediaRepresentationPayload::ManagedFileRef { .. }
    ));
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
    let ParsedAgentResponse {
        thought,
        tool_calls,
    } = ReActEngine::parse_default_model_response(&resp, 1);
    assert_eq!(thought, Some("Session done.".into()));
    assert_eq!(tool_calls.len(), 1);
    assert!(tool_calls[0].is_final);
    assert_eq!(tool_calls[0].tool_name, "final_answer");
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
    let ParsedAgentResponse {
        thought,
        tool_calls,
    } = ReActEngine::parse_default_model_response(&resp, 2);
    assert_eq!(thought, Some("Opening file.".into()));
    assert_eq!(tool_calls.len(), 1);
    assert!(!tool_calls[0].is_final);
    assert_eq!(tool_calls[0].tool_name, "open_file");
    assert_eq!(
        tool_calls[0].tool_input,
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
    let ParsedAgentResponse {
        thought,
        tool_calls,
    } = ReActEngine::parse_default_model_response(&resp, 1);
    assert_eq!(thought, Some("All done.".into()));
    assert_eq!(tool_calls.len(), 1);
    assert!(tool_calls[0].is_final);
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
    assert_eq!(executor.get_active_session_status(&session.id).await, None);

    let result = agent
        .process_input("more context", Some(session.id.clone()))
        .await
        .unwrap();
    assert!(matches!(result, ProcessResult::Supplemented { .. }));
    // Session is not reloaded into the working set and never becomes Pending.
    assert_eq!(executor.get_active_session_status(&session.id).await, None);
    assert!(executor.drain_follow_ups(&session.id).await.is_empty());
}

#[tokio::test]
async fn process_input_deletes_terminal_ghost_message_through_session_store() {
    let (agent, executor, db) = make_in_memory_agent();
    let session = executor.create_session("original").await.unwrap();
    executor.end_session(&session.id).await.unwrap();
    let terminal_status = db.get_session(&session.id).unwrap().unwrap().status;
    assert!(terminal_status.is_terminal());

    let bus = agent.events.install_bus();
    let updates = Arc::new(SessionUpdateCapture::default());
    bus.subscribe("terminal-ingress-test", updates.clone())
        .await;

    let result = agent
        .process_input("more context", Some(session.id.clone()))
        .await
        .unwrap();

    assert_eq!(result, ProcessResult::Supplemented { message_id: None });
    assert!(db.list_session_messages(&session.id).unwrap().is_empty());
    assert_eq!(
        db.get_session(&session.id).unwrap().unwrap().status,
        terminal_status
    );
    assert_eq!(executor.get_session_status(&session.id).await, None);
    assert_eq!(
        *updates.0.lock().unwrap(),
        vec![(session.id, terminal_status)]
    );
}

#[tokio::test]
async fn process_input_continues_when_terminal_ghost_delete_fails() {
    let (agent, executor, db) = make_in_memory_agent();
    let session = executor.create_session("original").await.unwrap();
    executor.end_session(&session.id).await.unwrap();
    let terminal_status = db.get_session(&session.id).unwrap().unwrap().status;
    assert!(terminal_status.is_terminal());
    db.conn()
        .execute_batch(
            r#"
            CREATE TRIGGER reject_ghost_message_delete
            BEFORE DELETE ON messages
            WHEN OLD.role = 'user' AND OLD.content = 'more context'
            BEGIN SELECT RAISE(ABORT, 'forced ghost delete failure'); END;
            "#,
        )
        .unwrap();

    let bus = agent.events.install_bus();
    let updates = Arc::new(SessionUpdateCapture::default());
    bus.subscribe("terminal-ingress-test", updates.clone())
        .await;

    let result = agent
        .process_input("more context", Some(session.id.clone()))
        .await
        .unwrap();

    assert_eq!(result, ProcessResult::Supplemented { message_id: None });
    let messages = db.list_session_messages(&session.id).unwrap();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].role, haven_common::types::CanonicalRole::User);
    assert_eq!(messages[0].content, "more context");
    assert!(
        agent
            .react_engine
            .event_store
            .pending_session_inputs(&session.id)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        db.get_session(&session.id).unwrap().unwrap().status,
        terminal_status
    );
    assert_eq!(executor.get_session_status(&session.id).await, None);
    assert_eq!(
        *updates.0.lock().unwrap(),
        vec![(session.id, terminal_status)]
    );
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
        executor.get_active_session_status(&session.id).await,
        Some(SessionStatus::Pending)
    );
    let supps: Vec<String> = executor
        .drain_follow_ups(&session.id)
        .await
        .into_iter()
        .map(|s| s.text)
        .collect();
    assert_eq!(supps, vec!["more context"]);
}

#[tokio::test]
async fn process_input_reserves_one_ask_answer_until_user_inject() {
    let (agent, executor) = make_test_agent();
    let session = executor.create_session("original").await.unwrap();
    executor
        .update_session_status(&session.id, SessionStatus::Paused)
        .await
        .unwrap();
    executor
        .request_interaction(crate::interaction::InteractionRequest::ask(
            &session.id,
            Vec::new(),
            vec!["step-0123456789abcdef0123456789abcdef".into()],
        ))
        .await
        .unwrap();
    let result = agent
        .process_input("the answer", Some(session.id.clone()))
        .await
        .unwrap();
    assert!(matches!(result, ProcessResult::Supplemented { .. }));

    // The Ask gate remains pending until UserInject commits. A second input
    // received before that acknowledgement is a follow-up, even though the
    // interaction registry still contains the Ask.
    agent
        .process_input("later input", Some(session.id.clone()))
        .await
        .unwrap();
    assert_eq!(
        executor.get_active_session_status(&session.id).await,
        Some(SessionStatus::Pending)
    );
    let supps = executor.drain_follow_ups(&session.id).await;
    assert_eq!(supps.len(), 2);
    assert!(
        supps[0].is_answer,
        "reply to an ask must be marked as answer"
    );
    assert_eq!(supps[0].text, "the answer");
    assert!(
        !supps[1].is_answer,
        "only one input reserves the Ask answer"
    );
    assert_eq!(supps[1].text, "later input");

    let pending = agent
        .react_engine
        .event_store
        .pending_session_inputs(&session.id)
        .await
        .unwrap();
    assert_eq!(pending.len(), 2);
    assert_eq!(
        pending[0].disposition,
        haven_memory::PendingInputDisposition::Answer
    );
    assert_eq!(
        pending[1].disposition,
        haven_memory::PendingInputDisposition::FollowUp
    );
    assert!(
        executor
            .pending_interactions(&session.id, crate::interaction::InteractionKind::Ask)
            .await
            .len()
            == 1,
        "ingress must leave Ask pending until its UserInject event commits"
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
    let supps = executor.drain_follow_ups(&session.id).await;
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
        executor.get_active_session_status(&session.id).await,
        Some(SessionStatus::Pending)
    );
    let supps = executor.drain_follow_ups(&session.id).await;
    assert_eq!(supps.len(), 1);
    assert_eq!(supps[0].text, "看图");
    assert_eq!(supps[0].attachments, vec![att]);

    // Persisted with attachments in the session's message stream.
    let msgs = agent.db.list_session_messages(&session.id).unwrap();
    let user_msg = msgs
        .iter()
        .find(|m| m.role == haven_common::types::CanonicalRole::User && m.content == "看图")
        .expect("user message persisted");
    assert_eq!(user_msg.attachments.len(), 1);
    assert_eq!(user_msg.attachments[0].media_type, "image/png");
    let pending = agent
        .react_engine
        .event_store
        .pending_session_inputs(&session.id)
        .await
        .unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].message.id, user_msg.id);
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
            let state = executor.get_active_session_status(&session_id).await;
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
    agent.memory_worker.infer_facts(&session.id).await;
}
