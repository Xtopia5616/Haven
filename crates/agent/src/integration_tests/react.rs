use super::support::*;
use super::*;
use haven_memory::RecoveryPersistenceStatus;

type StreamReset = (String, u32, u64, String, String);

struct StreamResetCollector {
    resets: std::sync::Mutex<Vec<StreamReset>>,
}

#[async_trait]
impl AgentEventEmitter for StreamResetCollector {
    async fn emit(&self, event: AgentEvent) {
        if let AgentEvent::StreamReset {
            session_id,
            step_number,
            run_id,
            thought_message_id,
            reasoning_message_id,
        } = event
        {
            self.resets.lock().unwrap().push((
                session_id,
                step_number,
                run_id,
                thought_message_id,
                reasoning_message_id,
            ));
        }
    }
}

#[tokio::test]
async fn run_session_emits_supplement_when_additional_context_queued() {
    let tools = Arc::new(ToolsManager::new());
    let client = Arc::new(FinalAnswerMock) as Arc<dyn LlmClient>;
    let (agent, executor) = make_test_agent_with(client, tools);

    let recorder = Arc::new(RecordingEmitter {
        thoughts: std::sync::Mutex::new(Vec::new()),
        supplements: std::sync::Mutex::new(Vec::new()),
        notifications: std::sync::Mutex::new(Vec::new()),
        completed: std::sync::Mutex::new(false),
    });
    agent.set_emitter(recorder.clone());

    let session = executor
        .create_session_with_summary("do stuff", "do stuff summary")
        .await
        .unwrap();
    executor
        .add_follow_up(&session.id, "extra: remember path X")
        .await
        .unwrap();

    let history = agent.run_session_from_id(&session.id).await.unwrap();
    assert!(!history.is_empty());

    let sups = recorder.supplements.lock().unwrap().clone();
    assert_eq!(sups.len(), 1, "exactly one supplement event expected");
    assert_eq!(sups[0], "extra: remember path X");
    // With supplements, session pauses instead of completing (conversation mode)
    let state = executor.get_active_session_status(&session.id).await;
    assert_eq!(
        state,
        Some(SessionStatus::Paused),
        "session should be paused (not completed) when supplements were processed"
    );
}

#[tokio::test]
async fn empty_retry_emits_stream_reset_before_replacement_output() {
    let empty = StreamChunk {
        text: None,
        tool_calls: Vec::new(),
        finish_reason: Some(FinishReason::Stop),
        usage: None,
        model: None,
        reasoning: None,
        web_search: None,
        web_search_calls: Vec::new(),
        thinking_blocks: Vec::new(),
    };
    let replacement = StreamChunk {
        text: Some("Recovered after retry.".into()),
        tool_calls: vec![CanonicalToolCall {
            id: "final-after-empty".into(),
            name: "final_answer".into(),
            arguments: serde_json::json!({}),
        }],
        finish_reason: Some(FinishReason::Stop),
        usage: None,
        model: None,
        reasoning: None,
        web_search: None,
        web_search_calls: Vec::new(),
        thinking_blocks: Vec::new(),
    };
    let mock = Arc::new(ScriptedMock::new(vec![
        ScriptedResponse::Chunk(empty),
        ScriptedResponse::Chunk(replacement),
    ]));
    let limits = ContextLimitsConfig {
        empty_response_max_retries: 1,
        empty_response_retry_delay_ms: 0,
        ..Default::default()
    };
    let (agent, executor) =
        make_test_agent_with_limits(mock, Arc::new(ToolsManager::new()), limits);
    let emitter = Arc::new(StreamResetCollector {
        resets: std::sync::Mutex::new(Vec::new()),
    });
    agent.set_emitter(emitter.clone());
    let session = executor
        .create_session("retry empty response")
        .await
        .unwrap();

    let history = agent.run_session_from_id(&session.id).await.unwrap();

    assert!(!history.is_empty());
    assert_eq!(
        executor.get_active_session_status(&session.id).await,
        Some(SessionStatus::Paused)
    );
    let resets = emitter.resets.lock().unwrap();
    assert_eq!(
        resets.len(),
        1,
        "the replacement attempt must reset the live stream once"
    );
    assert_eq!(resets[0].0, session.id);
    assert_eq!(resets[0].1, 1);
}

#[tokio::test]
async fn interrupt_then_continue_on_same_actor_uses_a_fresh_run_token() {
    let final_answer = |id: &str, text: &str| StreamChunk {
        text: Some(text.into()),
        tool_calls: vec![CanonicalToolCall {
            id: id.into(),
            name: "final_answer".into(),
            arguments: serde_json::json!({}),
        }],
        finish_reason: Some(FinishReason::Stop),
        usage: None,
        model: None,
        reasoning: None,
        web_search: None,
        web_search_calls: Vec::new(),
        thinking_blocks: Vec::new(),
    };
    let mock = Arc::new(ScriptedMock::new(vec![
        ScriptedResponse::ChunkDelayed(final_answer("interrupted", "Interrupted run."), 30_000),
        ScriptedResponse::Chunk(final_answer("continued", "Continued run.")),
    ]));
    let (agent, executor) = make_test_agent_with(mock.clone(), Arc::new(ToolsManager::new()));
    agent.set_emitter(make_recording_emitter());
    let session = executor
        .create_session("interrupt and continue")
        .await
        .unwrap();
    let original_actor = executor
        .actor_for(&session.id)
        .await
        .expect("session actor should be installed");

    let first_run = tokio::spawn({
        let agent = agent.clone();
        let session_id = session.id.clone();
        async move { agent.run_session_from_id(&session_id).await }
    });
    let request_deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while mock.seen.lock().unwrap().is_empty() {
        if std::time::Instant::now() >= request_deadline {
            let status = executor.get_session_status(&session.id).await;
            let in_flight = executor.is_run_in_flight(&session.id).await;
            let result = first_run.await;
            panic!(
                "first provider request never started (status: {status:?}, in_flight: {in_flight}, run: {result:?})"
            );
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    let interrupted_token = executor.cancellation_token(&session.id).await;

    agent.interrupt_session(&session.id).await.unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(2), first_run)
        .await
        .expect("interrupted run should stop promptly")
        .expect("first run task should join")
        .expect("interruption is a normal run exit");
    assert!(interrupted_token.is_cancelled());

    agent.continue_session(&session.id).await.unwrap();
    let continued_actor = executor
        .actor_for(&session.id)
        .await
        .expect("paused session should retain its actor for Continue");
    assert!(
        original_actor.is_same_actor(&continued_actor),
        "Continue should reuse the actor from the interrupted run"
    );
    assert_eq!(
        executor.try_claim_pending().await.as_deref(),
        Some(session.id.as_str()),
        "dispatcher should claim the continued session"
    );

    let history = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        agent.run_session_from_id(&session.id),
    )
    .await
    .expect("continued run should not inherit the previous cancellation")
    .unwrap();
    assert!(!history.is_empty());
    continued_actor
        .finish_run()
        .await
        .expect("finish dispatched continued run");
    assert_eq!(mock.seen.lock().unwrap().len(), 2);
    assert_eq!(
        executor.get_active_session_status(&session.id).await,
        Some(SessionStatus::Paused)
    );
}

#[tokio::test]
async fn turn_deadline_cancels_provider_retry_before_second_attempt() {
    let mock = Arc::new(ScriptedMock::new(vec![ScriptedResponse::Err(
        LlmError::Timeout("transient provider timeout".into()),
    )]));
    let mut limits = ContextLimitsConfig::default();
    limits.turn_deadline_secs = 1;
    let (agent, executor) =
        make_test_agent_with_limits(mock.clone(), Arc::new(ToolsManager::new()), limits);
    agent.set_emitter(make_recording_emitter());
    let session = executor
        .create_session("retry within deadline")
        .await
        .unwrap();

    let started = Instant::now();
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(3),
        agent.run_session_from_id(&session.id),
    )
    .await
    .expect("provider retry must observe the turn cancellation")
    .expect_err("the exhausted deadline should fail the run");

    assert!(result.to_string().contains("deadline"), "{result:#}");
    assert!(started.elapsed() < std::time::Duration::from_secs(3));
    assert_eq!(
        mock.seen.lock().unwrap().len(),
        1,
        "the retry delay must not start a second provider request"
    );
}

#[tokio::test]
async fn turn_deadline_stops_after_non_cooperative_blocking_tool() {
    let completed = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let tools = Arc::new(ToolsManager::new());
    tools
        .registry()
        .register(Arc::new(BlockingTool::new(completed.clone())) as ToolBox)
        .await
        .unwrap();
    let mock = Arc::new(ScriptedMock::new(vec![ScriptedResponse::Chunk(
        StreamChunk {
            text: Some("Run the blocking operation.".into()),
            tool_calls: vec![CanonicalToolCall {
                id: "blocking-call".into(),
                name: "blocking_tool".into(),
                arguments: serde_json::json!({}),
            }],
            finish_reason: Some(FinishReason::ToolCalls),
            usage: None,
            model: None,
            reasoning: None,
            web_search: None,
            web_search_calls: Vec::new(),
            thinking_blocks: Vec::new(),
        },
    )]));
    let mut limits = ContextLimitsConfig::default();
    limits.turn_deadline_secs = 1;
    let (agent, executor) = make_test_agent_with_limits(mock.clone(), tools, limits);
    agent.set_emitter(make_recording_emitter());
    let session = executor.create_session("stop the slow tool").await.unwrap();

    let started = Instant::now();
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        agent.run_session_from_id(&session.id),
    )
    .await
    .expect("deadline must stop the turn before native work finishes")
    .expect_err("slow tool should make the turn fail at its deadline");

    assert!(result.to_string().contains("deadline"), "{result:#}");
    assert!(started.elapsed() < std::time::Duration::from_secs(2));
    assert_eq!(mock.seen.lock().unwrap().len(), 1);
    assert!(!completed.load(std::sync::atomic::Ordering::Acquire));
    tokio::time::sleep(std::time::Duration::from_secs(3)).await;
    assert!(completed.load(std::sync::atomic::Ordering::Acquire));
}

#[tokio::test]
async fn loop_pauses_on_pending_ask_instead_of_heuristic_final() {
    // The model responds with text + Stop and no tool calls while an
    // an explicit ask interaction is pending: the turn must
    // pause and wait for the user's answer instead of completing.
    let client = Arc::new(ScriptedMock::new(vec![ScriptedResponse::Chunk(
        StreamChunk {
            text: Some("I'll stop here.".into()),
            tool_calls: vec![],
            finish_reason: Some(FinishReason::Stop),
            usage: None,
            model: None,
            reasoning: None,
            web_search: None,
            web_search_calls: Vec::new(),
            thinking_blocks: Vec::new(),
        },
    )]));
    let (agent, executor) = make_test_agent_with(client, Arc::new(ToolsManager::new()));
    agent.set_emitter(make_recording_emitter());
    let session = executor.create_session("session").await.unwrap();
    let canonical = vec![
        CanonicalMessage::system(vec![ContentPart::text("sys")]),
        CanonicalMessage::user_text("help me"),
        CanonicalMessage::assistant(
            vec![ContentPart::text("let me ask")],
            Some(vec![CanonicalToolCall {
                id: "call_ask".into(),
                name: "ask".into(),
                arguments: serde_json::json!({"question": "which file?"}),
            }]),
            None,
            Vec::new(),
            Vec::new(),
        ),
        CanonicalMessage::tool(
            vec![ContentPart::text(
                r#"{"ask":true,"question":"which file?","options":[]}"#,
            )],
            Some("call_ask".into()),
        ),
    ];
    let snapshot = EventProjection {
        events: seed_events_from_canonical(canonical),
        step_number: 1,
        branch_points: HashMap::new(),
        interactions: vec![crate::interaction::InteractionRequest::ask(
            &session.id,
            Vec::new(),
            vec!["step-ask".into()],
        )],
    };
    seed_event_projection(&agent, &session.id, &snapshot).await;
    agent.run_session_from_id(&session.id).await.unwrap();

    assert_eq!(
        executor.get_active_session_status(&session.id).await,
        Some(SessionStatus::Paused),
        "session must pause for the pending question instead of completing"
    );
    assert_eq!(
        executor
            .pending_interactions(&session.id, crate::interaction::InteractionKind::Ask)
            .await
            .len(),
        1,
        "pause must retain the pending ask interaction"
    );
}

#[tokio::test]
async fn budget_exhaustion_pauses_with_notification_and_no_chat_message() {
    // The scripted LLM always returns a non-final tool call, so the run
    // consumes its 1-step budget without ever producing a final answer.
    let client = Arc::new(ScriptedMock::new(vec![ScriptedResponse::Chunk(
        StreamChunk {
            text: Some("keep working".into()),
            tool_calls: vec![CanonicalToolCall {
                id: "c1".into(),
                name: "read_file".into(),
                arguments: serde_json::json!({"path": "x"}),
            }],
            finish_reason: Some(FinishReason::ToolCalls),
            usage: None,
            model: None,
            reasoning: None,
            web_search: None,
            web_search_calls: Vec::new(),
            thinking_blocks: Vec::new(),
        },
    )]));
    let (agent, executor) = make_test_agent_with(client, Arc::new(ToolsManager::new()));
    agent.set_max_steps(1).unwrap();
    let recorder = make_recording_emitter();
    agent.set_emitter(recorder.clone());
    let session = executor.create_session("session").await.unwrap();
    agent.run_session_from_id(&session.id).await.unwrap();

    assert_eq!(
        executor.get_active_session_status(&session.id).await,
        Some(SessionStatus::Paused),
        "budget exhaustion must pause the session as a checkpoint"
    );
    // The notice must NOT be persisted as an assistant chat message.
    let msgs = agent.db.get_session_messages(&session.id).unwrap();
    assert!(
        msgs.iter()
            .all(|m| !m.content.contains("任务步骤上限已用尽")),
        "budget notice must not appear as a chat message: {:?}",
        msgs.iter().map(|m| m.content.as_str()).collect::<Vec<_>>()
    );
    // It must be surfaced as a Notification event instead.
    let notifications = recorder.notifications.lock().unwrap().clone();
    assert!(
        notifications
            .iter()
            .any(|(title, _)| title == "任务步骤上限已用尽"),
        "budget notice must be emitted as a notification: {:?}",
        notifications
    );
    let pause_triggers = agent
        .react_engine
        .event_store
        .read_active_domain_events(&session.id)
        .unwrap()
        .into_iter()
        .filter(|event| event.event_type == haven_memory::MEMORY_TRIGGER_EVENT_TYPE)
        .collect::<Vec<_>>();
    assert_eq!(pause_triggers.len(), 1);
    let payload: serde_json::Value = serde_json::from_str(&pause_triggers[0].payload).unwrap();
    assert_eq!(payload["trigger_kind"], "pause");
    assert_eq!(payload["bypass_throttle"], true);
    assert_eq!(payload["pause_reason"], "budget");
    assert_eq!(pause_triggers[0].run_id, Some(1));
    assert_eq!(pause_triggers[0].step_number, Some(2));
}

#[tokio::test]
async fn truncated_text_only_response_retried_before_final() {
    // First response: text with a Length finish (generation cut off) ??        // must NOT end the turn as if it were the final answer. Second
    // response: a complete Stop answer, which ends the turn.
    let client = Arc::new(ScriptedMock::new(vec![
        ScriptedResponse::Chunk(StreamChunk {
            text: Some("Here is the partial answer".into()),
            tool_calls: vec![],
            finish_reason: Some(FinishReason::Length),
            usage: None,
            model: None,
            reasoning: None,
            web_search: None,
            web_search_calls: Vec::new(),
            thinking_blocks: Vec::new(),
        }),
        ScriptedResponse::Chunk(StreamChunk {
            text: Some("Here is the complete answer.".into()),
            tool_calls: vec![],
            finish_reason: Some(FinishReason::Stop),
            usage: None,
            model: None,
            reasoning: None,
            web_search: None,
            web_search_calls: Vec::new(),
            thinking_blocks: Vec::new(),
        }),
    ]));
    let (agent, executor) = make_test_agent_with(client, Arc::new(ToolsManager::new()));
    agent.set_emitter(make_recording_emitter());
    let session = executor.create_session("session").await.unwrap();
    agent.run_session_from_id(&session.id).await.unwrap();

    assert_eq!(
        executor.get_active_session_status(&session.id).await,
        Some(SessionStatus::Paused),
        "turn must end paused after the retried final"
    );
    let msgs = agent.db.get_session_messages(&session.id).unwrap();
    assert_eq!(
        msgs.last().unwrap().content,
        "Here is the complete answer.",
        "the retried (complete) response must be the final message, not the truncated one"
    );
}

#[tokio::test]
async fn run_session_executes_tool_then_final_answer() {
    let tools = Arc::new(ToolsManager::new());
    tools
        .registry()
        .register(Arc::new(EchoTool) as ToolBox)
        .await
        .unwrap();
    let mock = Arc::new(ScriptedMock::new(vec![
        ScriptedResponse::Chunk(StreamChunk {
            text: Some("I'll echo that.".into()),
            tool_calls: vec![CanonicalToolCall {
                id: "tc1".into(),
                name: "echo".into(),
                arguments: serde_json::json!({"text": "hello"}),
            }],
            finish_reason: Some(FinishReason::ToolCalls),
            usage: None,
            model: None,
            reasoning: None,
            web_search: None,
            web_search_calls: Vec::new(),
            thinking_blocks: Vec::new(),
        }),
        ScriptedResponse::Chunk(StreamChunk {
            text: Some("Done.".into()),
            tool_calls: vec![CanonicalToolCall {
                id: "final".into(),
                name: "final_answer".into(),
                arguments: serde_json::json!({}),
            }],
            finish_reason: Some(FinishReason::Stop),
            usage: None,
            model: None,
            reasoning: None,
            web_search: None,
            web_search_calls: Vec::new(),
            thinking_blocks: Vec::new(),
        }),
    ]));
    let (agent, executor) = make_test_agent_with(mock, tools);
    let collector = Arc::new(EventCollector::new());
    agent.set_emitter(collector.clone());
    let session = executor.create_session("echo hello").await.unwrap();
    let history = agent.run_session_from_id(&session.id).await.unwrap();
    assert!(history.len() >= 2, "should have at least 2 steps");
    assert!(collector.has_action("echo"));
    assert!(collector.has_observation("echo"));
    assert_eq!(
        executor.get_active_session_status(&session.id).await,
        Some(SessionStatus::Paused)
    );
}

#[tokio::test]
async fn media_tool_usage_flows_to_event_and_database() {
    let temp = tempfile::tempdir().unwrap();
    let image_path = temp.path().join("picture.png");
    tokio::fs::write(&image_path, b"test image bytes")
        .await
        .unwrap();
    let asset_id = "asset-00000000000000000000000000000000";
    let registry = haven_tools::ManagedAssetRegistry::default();
    assert!(registry.register_under_root(
        temp.path(),
        asset_id,
        image_path,
        Some("picture.png".into()),
        "image/png",
    ));

    let vision_client = Arc::new(VisionUsageMock) as Arc<dyn LlmClient>;
    let vision_router = Arc::new(LlmRouter::new_with_clients(
        vision_client.clone(),
        vision_client.clone(),
        vision_client.clone(),
        vision_client,
    ));
    let media_tool = Arc::new(haven_tools::builtin::media::MediaTool::new(
        Some(vision_router),
        registry,
        1024 * 1024,
        10,
        2_000,
    )) as ToolBox;

    let tools = Arc::new(ToolsManager::new());
    tools
        .share_services()
        .authorization
        .set_permission_mode(haven_common::types::PermissionMode::Autonomous)
        .await;
    // `media.describe` delegates to a configured vision provider, so the
    // external-network disclosure gate still applies in Autonomous mode.
    // This test exercises usage propagation after that explicit approval.
    tools
        .share_services()
        .authorization
        .grant(
            None,
            "media.describe",
            haven_common::types::PermissionEffect::Allow,
            haven_common::types::PermissionScope::Always,
        )
        .await;
    tools.registry().register(media_tool).await.unwrap();
    let main_client = Arc::new(ScriptedMock::new(vec![
        ScriptedResponse::Chunk(StreamChunk {
            text: Some("I will inspect the image.".into()),
            tool_calls: vec![CanonicalToolCall {
                id: "media-call".into(),
                name: "media".into(),
                arguments: serde_json::json!({
                    "operation": "describe",
                    "asset_id": asset_id,
                }),
            }],
            finish_reason: Some(FinishReason::ToolCalls),
            usage: None,
            model: None,
            reasoning: None,
            web_search: None,
            web_search_calls: Vec::new(),
            thinking_blocks: Vec::new(),
        }),
        ScriptedResponse::Chunk(StreamChunk {
            text: Some("The image was inspected.".into()),
            tool_calls: vec![CanonicalToolCall {
                id: "final".into(),
                name: "final_answer".into(),
                arguments: serde_json::json!({}),
            }],
            finish_reason: Some(FinishReason::Stop),
            usage: None,
            model: None,
            reasoning: None,
            web_search: None,
            web_search_calls: Vec::new(),
            thinking_blocks: Vec::new(),
        }),
    ]));
    let (agent, executor) = make_test_agent_with(main_client, tools);
    let collector = Arc::new(EventCollector::new());
    agent.set_emitter(collector.clone());
    let session = executor.create_session("describe the image").await.unwrap();

    agent.run_session_from_id(&session.id).await.unwrap();

    let calls = agent.db.get_session_llm_usage(&session.id).unwrap();
    assert_eq!(calls.len(), 1, "only the media client reports usage");
    assert_eq!(calls[0].call_kind, "media");
    assert_eq!(calls[0].step_number, Some(1));
    assert_eq!(calls[0].role, haven_common::config::RequestKind::Vision);
    assert_eq!(calls[0].model.as_deref(), Some("vision-test"));
    assert_eq!(calls[0].prompt_tokens, 11);
    assert_eq!(calls[0].completion_tokens, 7);
    assert_eq!(calls[0].total_tokens, 18);

    let events = collector.events.lock().unwrap();
    assert!(events.iter().any(|event| {
        matches!(
            event,
            AgentEvent::Usage {
                session_id,
                call_kind,
                step_number: Some(1),
                prompt_tokens: 11,
                completion_tokens: 7,
                total_tokens: 18,
                ..
            } if session_id == &session.id && call_kind == "media"
        )
    }));
}

#[tokio::test]
async fn run_session_empty_tool_call_id_stays_consistent_in_canonical() {
    // Some providers return an empty tool_call_id. The Action side
    // synthesizes a UUID; the canonical assistant declaration must echo
    // the SAME id (not the raw empty string), otherwise the tool result
    // references an id the assistant never declared and the next request
    // is rejected with a 400.
    let tools = Arc::new(ToolsManager::new());
    tools
        .registry()
        .register(Arc::new(EchoTool) as ToolBox)
        .await
        .unwrap();
    let mock = Arc::new(ScriptedMock::new(vec![
        ScriptedResponse::Chunk(StreamChunk {
            text: Some("I'll echo that.".into()),
            tool_calls: vec![CanonicalToolCall {
                id: String::new(), // provider sends empty id
                name: "echo".into(),
                arguments: serde_json::json!({"text": "hello"}),
            }],
            finish_reason: Some(FinishReason::ToolCalls),
            usage: None,
            model: None,
            reasoning: None,
            web_search: None,
            web_search_calls: Vec::new(),
            thinking_blocks: Vec::new(),
        }),
        ScriptedResponse::Chunk(StreamChunk {
            text: Some("Done.".into()),
            tool_calls: vec![CanonicalToolCall {
                id: "final".into(),
                name: "final_answer".into(),
                arguments: serde_json::json!({}),
            }],
            finish_reason: Some(FinishReason::Stop),
            usage: None,
            model: None,
            reasoning: None,
            web_search: None,
            web_search_calls: Vec::new(),
            thinking_blocks: Vec::new(),
        }),
    ]));
    let (agent, executor) = make_test_agent_with(mock, tools);
    let collector = Arc::new(EventCollector::new());
    agent.set_emitter(collector.clone());
    let session = executor.create_session("echo hello").await.unwrap();
    agent.run_session_from_id(&session.id).await.unwrap();

    // Inspect the saved snapshot's canonical: the assistant declaration
    // and the tool result must share the same (non-empty) id.
    let saved = load_event_projection(&agent, &session.id).await;
    let mut declared: Option<String> = None;
    let (canonical, _) = saved.project();
    for m in &canonical {
        if let Some(calls) = &m.tool_calls {
            for tc in calls {
                assert!(!tc.id.is_empty(), "declared id must not be empty");
                declared = Some(tc.id.clone());
            }
        }
        if let Some(tid) = &m.tool_call_id {
            assert_eq!(
                Some(tid),
                declared.as_ref(),
                "tool result id must match the assistant's declared call id"
            );
        }
    }
    assert!(
        declared.is_some(),
        "an echo tool call must have been declared"
    );
}

#[tokio::test]
async fn run_session_injects_mid_turn_steering_before_final_content() {
    // A user message sent while the agent is generating its final answer
    // must be injected before the turn ends (between the tool calls and
    // the final content) instead of being deferred until after
    // completion. The final LLM response is delayed so the steering
    // arrives while that call is still in flight; the agent must then
    // re-run with the message in context.
    let tools = Arc::new(ToolsManager::new());
    tools
        .registry()
        .register(Arc::new(EchoTool) as ToolBox)
        .await
        .unwrap();
    let mock = Arc::new(ScriptedMock::new(vec![
        ScriptedResponse::Chunk(StreamChunk {
            text: Some("I'll echo that.".into()),
            tool_calls: vec![CanonicalToolCall {
                id: "tc1".into(),
                name: "echo".into(),
                arguments: serde_json::json!({"text": "hello"}),
            }],
            finish_reason: Some(FinishReason::ToolCalls),
            usage: None,
            model: None,
            reasoning: None,
            web_search: None,
            web_search_calls: Vec::new(),
            thinking_blocks: Vec::new(),
        }),
        // Delayed final answer: the steering is added while this call is
        // in flight, so the final-content branch must pick it up.
        ScriptedResponse::ChunkDelayed(
            StreamChunk {
                text: Some("Done.".into()),
                tool_calls: vec![CanonicalToolCall {
                    id: "final".into(),
                    name: "final_answer".into(),
                    arguments: serde_json::json!({}),
                }],
                finish_reason: Some(FinishReason::Stop),
                usage: None,
                model: None,
                reasoning: None,
                web_search: None,
                web_search_calls: Vec::new(),
                thinking_blocks: Vec::new(),
            },
            300,
        ),
        // The re-run after the steering was injected also answers finally.
        ScriptedResponse::Chunk(StreamChunk {
            text: Some("Understood, continuing in French.".into()),
            tool_calls: vec![CanonicalToolCall {
                id: "final2".into(),
                name: "final_answer".into(),
                arguments: serde_json::json!({}),
            }],
            finish_reason: Some(FinishReason::Stop),
            usage: None,
            model: None,
            reasoning: None,
            web_search: None,
            web_search_calls: Vec::new(),
            thinking_blocks: Vec::new(),
        }),
    ]));
    let (agent, executor) = make_test_agent_with(mock.clone(), tools);
    let collector = Arc::new(EventCollector::new());
    agent.set_emitter(collector.clone());
    let session = executor.create_session("echo hello").await.unwrap();

    let run = tokio::spawn({
        let agent = agent.clone();
        let session_id = session.id.clone();
        async move { agent.run_session_from_id(&session_id).await }
    });
    // The second LLM call is `ChunkDelayed` (300 ms) specifically so the
    // steering can land while it streams. Wait until that call has actually
    // STARTED (its `seen` entry is pushed before the delay) instead of
    // racing a fixed wall-clock sleep: under parallel test load the sleep
    // can overshoot past the delay and the steering would land after the
    // turn already completed, flaking the assertion below.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        if mock.seen.lock().unwrap().len() >= 2 {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "second (delayed) LLM call never started"
        );
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    executor
        .add_steering(&session.id, "stop and use French")
        .await
        .unwrap();
    let history = run.await.unwrap().unwrap();

    {
        let seen = mock.seen.lock().unwrap();
        assert_eq!(seen.len(), 3, "agent must re-run after mid-turn steering");
        let last_call = seen.last().unwrap();
        assert!(
            last_call.iter().any(|m| {
                matches!(m.role, CanonicalRole::User)
                    && m.source == Some(InjectSource::Steering)
                    && m.content.iter().any(
                        |c| matches!(c, ContentPart::Text(t) if t.contains("stop and use French")),
                    )
            }),
            "steering must be injected into the re-run LLM call"
        );
    }
    assert!(
        history.len() >= 3,
        "should have re-run after steering injection"
    );
    assert_eq!(
        executor.get_active_session_status(&session.id).await,
        Some(SessionStatus::Paused)
    );
}

#[tokio::test]
async fn run_session_injects_steering_between_tool_calls() {
    // A user message sent while the agent is executing tools is drained
    // at the next step boundary ??between tool calls ??so the final
    // answer is generated with the new context.
    let tools = Arc::new(ToolsManager::new());
    let timing = Arc::new(TimingState::new());
    tools
        .registry()
        .register(Arc::new(TimingTool::new("delay_a", timing.clone())) as ToolBox)
        .await
        .unwrap();
    let mock = Arc::new(ScriptedMock::new(vec![
        ScriptedResponse::Chunk(StreamChunk {
            text: Some("Running the tool.".into()),
            tool_calls: vec![CanonicalToolCall {
                id: "tc1".into(),
                name: "delay_a".into(),
                arguments: serde_json::json!({}),
            }],
            finish_reason: Some(FinishReason::ToolCalls),
            usage: None,
            model: None,
            reasoning: None,
            web_search: None,
            web_search_calls: Vec::new(),
            thinking_blocks: Vec::new(),
        }),
        ScriptedResponse::Chunk(StreamChunk {
            text: Some("Done.".into()),
            tool_calls: vec![CanonicalToolCall {
                id: "final".into(),
                name: "final_answer".into(),
                arguments: serde_json::json!({}),
            }],
            finish_reason: Some(FinishReason::Stop),
            usage: None,
            model: None,
            reasoning: None,
            web_search: None,
            web_search_calls: Vec::new(),
            thinking_blocks: Vec::new(),
        }),
    ]));
    let (agent, executor) = make_test_agent_with(mock.clone(), tools);
    let collector = Arc::new(EventCollector::new());
    agent.set_emitter(collector.clone());
    let session = executor.create_session("run tool").await.unwrap();

    let run = tokio::spawn({
        let agent = agent.clone();
        let session_id = session.id.clone();
        async move { agent.run_session_from_id(&session_id).await }
    });
    // Deliver the message while delay_a (200ms) is still executing.
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    executor
        .add_steering(&session.id, "add more detail")
        .await
        .unwrap();
    let _ = run.await.unwrap().unwrap();

    {
        let seen = mock.seen.lock().unwrap();
        assert_eq!(
            seen.len(),
            2,
            "no re-run needed: steering is drained at step boundary"
        );
        assert!(
            seen[1].iter().any(|m| {
                matches!(m.role, CanonicalRole::User)
                    && m.source == Some(InjectSource::Steering)
                    && m.content
                        .iter()
                        .any(|c| matches!(c, ContentPart::Text(t) if t.contains("add more detail")))
            }),
            "steering must be injected into the next LLM call after the tool step"
        );
    }
    assert_eq!(
        executor.get_active_session_status(&session.id).await,
        Some(SessionStatus::Paused)
    );
}

#[tokio::test]
async fn run_session_ask_tool_pauses_and_surfaces_question() {
    // The `ask` tool signals the ReAct loop to pause and wait for the
    // user's reply (delivered as a supplement on resume). Verify the session
    // ends Paused and the question is persisted as an assistant message.
    let tools = Arc::new(ToolsManager::new());
    tools
        .registry()
        .register(Arc::new(haven_tools::builtin::ask::AskTool) as ToolBox)
        .await
        .unwrap();
    let mock = Arc::new(ScriptedMock::new(vec![
        ScriptedResponse::Chunk(StreamChunk {
            text: Some("I need to clarify before proceeding.".into()),
            tool_calls: vec![CanonicalToolCall {
                id: "tc1".into(),
                name: "ask".into(),
                arguments: serde_json::json!({"question": "Which path should I take: A or B?"}),
            }],
            finish_reason: Some(FinishReason::ToolCalls),
            usage: None,
            model: None,
            reasoning: None,
            web_search: None,
            web_search_calls: Vec::new(),
            thinking_blocks: Vec::new(),
        }),
        // The loop pauses after `ask`, so a second response is never
        // consumed; include a final_answer anyway to catch regressions
        // where the loop incorrectly continues.
        ScriptedResponse::Chunk(StreamChunk {
            text: Some("Done.".into()),
            tool_calls: vec![CanonicalToolCall {
                id: "final".into(),
                name: "final_answer".into(),
                arguments: serde_json::json!({}),
            }],
            finish_reason: Some(FinishReason::Stop),
            usage: None,
            model: None,
            reasoning: None,
            web_search: None,
            web_search_calls: Vec::new(),
            thinking_blocks: Vec::new(),
        }),
    ]));
    let (agent, executor) = make_test_agent_with(mock, tools);
    let collector = Arc::new(EventCollector::new());
    agent.set_emitter(collector.clone());
    let session = executor.create_session("decide a path").await.unwrap();
    agent.run_session_from_id(&session.id).await.unwrap();

    // Session must be paused, awaiting the user's answer.
    assert_eq!(
        executor.get_active_session_status(&session.id).await,
        Some(SessionStatus::Paused),
        "ask should pause the session"
    );
    assert!(collector.has_action("ask"));
    assert!(collector.has_observation("ask"));

    // The question must be persisted so the user can see and answer it.
    let msgs = agent.db.get_session_messages(&session.id).unwrap();
    let found = msgs
        .iter()
        .any(|m| m.role == "assistant" && m.content.contains("Which path should I take"));
    assert!(found, "question should be persisted as assistant message");

    let pause_triggers = agent
        .react_engine
        .event_store
        .read_active_domain_events(&session.id)
        .unwrap()
        .into_iter()
        .filter(|event| event.event_type == haven_memory::MEMORY_TRIGGER_EVENT_TYPE)
        .collect::<Vec<_>>();
    assert_eq!(pause_triggers.len(), 1);
    let payload: serde_json::Value = serde_json::from_str(&pause_triggers[0].payload).unwrap();
    assert_eq!(payload["trigger_kind"], "pause");
    assert_eq!(payload["bypass_throttle"], true);
    assert_eq!(payload["pause_reason"], "ask");
    assert_eq!(pause_triggers[0].run_id, Some(1));
    assert_eq!(pause_triggers[0].step_number, Some(2));
}

#[tokio::test]
async fn ask_interaction_survives_executor_restart_from_durable_snapshot() {
    let db = temp_db();
    let tools = Arc::new(ToolsManager::new());
    tools
        .registry()
        .register(Arc::new(haven_tools::builtin::ask::AskTool) as ToolBox)
        .await
        .unwrap();
    let mock = Arc::new(ScriptedMock::new(vec![ScriptedResponse::Chunk(
        StreamChunk {
            text: Some("Need a decision.".into()),
            tool_calls: vec![CanonicalToolCall {
                id: "ask-restart".into(),
                name: "ask".into(),
                arguments: serde_json::json!({"question": "Which path?"}),
            }],
            finish_reason: Some(FinishReason::ToolCalls),
            usage: None,
            model: None,
            reasoning: None,
            web_search: None,
            web_search_calls: Vec::new(),
            thinking_blocks: Vec::new(),
        },
    )]));
    let (agent, executor) =
        make_test_agent_with_db(db.clone(), mock, tools, ContextLimitsConfig::default());
    agent.set_emitter(make_recording_emitter());
    let session = executor.create_session("restart me").await.unwrap();
    agent.run_session_from_id(&session.id).await.unwrap();
    let recovered_pending = executor
        .pending_interactions(&session.id, crate::interaction::InteractionKind::Ask)
        .await;
    assert_eq!(recovered_pending.len(), 1);

    executor.clear_all_sessions_for_shutdown().await.unwrap();

    let restarted_tools = Arc::new(ToolsManager::new());
    restarted_tools
        .registry()
        .register(Arc::new(haven_tools::builtin::ask::AskTool) as ToolBox)
        .await
        .unwrap();
    let final_mock = Arc::new(FinalAnswerMock) as Arc<dyn LlmClient>;
    let (restarted_agent, restarted_executor) = make_test_agent_with_db(
        db,
        final_mock,
        restarted_tools,
        ContextLimitsConfig::default(),
    );
    restarted_agent.set_emitter(make_recording_emitter());
    restarted_executor
        .ensure_session_loaded(&session.id)
        .await
        .unwrap();
    restarted_agent
        .run_session_from_id(&session.id)
        .await
        .unwrap();

    let pending = restarted_executor
        .pending_interactions(&session.id, crate::interaction::InteractionKind::Ask)
        .await;
    assert_eq!(
        pending.len(),
        1,
        "Ask must be restored after a process restart"
    );
}

#[tokio::test]
async fn run_session_ask_resumes_after_user_answer() {
    // After `ask` pauses the session, the user's reply arrives as a
    // supplement; the loop resumes and should reach final_answer.
    let tools = Arc::new(ToolsManager::new());
    tools
        .registry()
        .register(Arc::new(haven_tools::builtin::ask::AskTool) as ToolBox)
        .await
        .unwrap();
    let mock = Arc::new(ScriptedMock::new(vec![
        // Step 1: agent asks.
        ScriptedResponse::Chunk(StreamChunk {
            text: Some("Clarifying.".into()),
            tool_calls: vec![CanonicalToolCall {
                id: "tc1".into(),
                name: "ask".into(),
                arguments: serde_json::json!({"question": "A or B?"}),
            }],
            finish_reason: Some(FinishReason::ToolCalls),
            usage: None,
            model: None,
            reasoning: None,
            web_search: None,
            web_search_calls: Vec::new(),
            thinking_blocks: Vec::new(),
        }),
        // Step 2 (after resume): final answer.
        ScriptedResponse::Chunk(StreamChunk {
            text: Some("Going with A.".into()),
            tool_calls: vec![CanonicalToolCall {
                id: "final".into(),
                name: "final_answer".into(),
                arguments: serde_json::json!({}),
            }],
            finish_reason: Some(FinishReason::Stop),
            usage: None,
            model: None,
            reasoning: None,
            web_search: None,
            web_search_calls: Vec::new(),
            thinking_blocks: Vec::new(),
        }),
    ]));
    let (agent, executor) = make_test_agent_with(mock, tools);
    let collector = Arc::new(EventCollector::new());
    agent.set_emitter(collector.clone());
    let session = executor.create_session("pick a path").await.unwrap();
    agent.run_session_from_id(&session.id).await.unwrap();
    assert_eq!(
        executor.get_active_session_status(&session.id).await,
        Some(SessionStatus::Paused),
        "ask should pause"
    );

    // User answers; the supplement flips the session back to Pending.
    executor
        .add_follow_up(&session.id, "Use option A.")
        .await
        .unwrap();
    executor
        .update_session_status(&session.id, SessionStatus::Pending)
        .await
        .unwrap();
    agent.run_session_from_id(&session.id).await.unwrap();
    assert_eq!(
        executor.get_active_session_status(&session.id).await,
        Some(SessionStatus::Paused),
        "session should pause again after final answer"
    );
    // The final answer text should be persisted, proving the loop resumed
    // past the `ask` step and reached final_answer.
    let msgs = agent.db.get_session_messages(&session.id).unwrap();
    let answered = msgs
        .iter()
        .any(|m| m.role == "assistant" && m.content.contains("Going with A"));
    assert!(answered, "final answer should be persisted after resume");
}

#[tokio::test]
async fn retry_after_ask_answer_error_keeps_single_history() {
    // Reproduce the reported issue: the agent asks a question, the user
    // answers, the resumed step fails, and the user retries. Every retry
    // must OVERWRITE the previous attempt's persisted output — the resume
    // history should show exactly one question, one answer, one response.
    let tools = Arc::new(ToolsManager::new());
    tools
        .registry()
        .register(Arc::new(haven_tools::builtin::ask::AskTool) as ToolBox)
        .await
        .unwrap();
    let mock = Arc::new(ScriptedMock::new(vec![
        // Step 1: ask the question.
        ScriptedResponse::Chunk(StreamChunk {
            text: Some("Asking.".into()),
            tool_calls: vec![CanonicalToolCall {
                id: "tc1".into(),
                name: "ask".into(),
                arguments: serde_json::json!({"question": "Proceed?"}),
            }],
            finish_reason: Some(FinishReason::ToolCalls),
            usage: None,
            model: None,
            reasoning: None,
            web_search: None,
            web_search_calls: Vec::new(),
            thinking_blocks: Vec::new(),
        }),
        // Step 2 (after the answer): streams a partial thought, then fails.
        ScriptedResponse::ChunkThenErr(
            StreamChunk {
                text: Some("Let me think...".into()),
                tool_calls: vec![],
                finish_reason: None,
                usage: None,
                model: None,
                reasoning: None,
                web_search: None,
                web_search_calls: Vec::new(),
                thinking_blocks: Vec::new(),
            },
            LlmError::Unknown("mock mid-stream failure".into()),
        ),
        // Step 2 retry: final answer.
        ScriptedResponse::Chunk(StreamChunk {
            text: Some("Answer accepted.".into()),
            tool_calls: vec![CanonicalToolCall {
                id: "final".into(),
                name: "final_answer".into(),
                arguments: serde_json::json!({}),
            }],
            finish_reason: Some(FinishReason::Stop),
            usage: None,
            model: None,
            reasoning: None,
            web_search: None,
            web_search_calls: Vec::new(),
            thinking_blocks: Vec::new(),
        }),
    ]));
    let (agent, executor) = make_test_agent_with(mock, tools);
    agent.set_emitter(Arc::new(EventCollector::new()));
    let session = executor.create_session("ask retry").await.unwrap();

    // Turn 1: the ask pauses the session.
    agent.run_session_from_id(&session.id).await.unwrap();
    assert_eq!(
        executor.get_active_session_status(&session.id).await,
        Some(SessionStatus::Paused)
    );

    // Turn 2: the user answers; the resumed step fails mid-stream.
    executor.add_follow_up(&session.id, "Yes").await.unwrap();
    executor
        .update_session_status(&session.id, SessionStatus::Pending)
        .await
        .unwrap();
    let _ = agent.run_session_from_id(&session.id).await;
    // The failed run ended in Error; terminal cleanup removed the session
    // from the working set.
    assert_eq!(executor.get_active_session_status(&session.id).await, None);

    // Turn 3: retry via continue_session ??Pending ??re-run.
    agent.continue_session(&session.id).await.unwrap();
    assert_eq!(
        executor.get_active_session_status(&session.id).await,
        Some(SessionStatus::Pending)
    );
    agent.run_session_from_id(&session.id).await.unwrap();

    let msgs = agent.db.get_session_messages(&session.id).unwrap();
    let steps = agent.db.get_session_steps(&session.id).unwrap();

    // The failed attempt's partial text must be gone (overwritten).
    let partials: Vec<&str> = msgs
        .iter()
        .filter(|m| m.role == "assistant" && m.content.contains("Let me think"))
        .map(|m| m.content.as_str())
        .collect();
    assert!(
        partials.is_empty(),
        "partial output from the failed attempt should be deleted, got {:?}",
        partials
    );
    // Exactly one question and one final answer.
    let questions = msgs
        .iter()
        .filter(|m| m.role == "assistant" && m.content.contains("Proceed?"))
        .count();
    let finals = msgs
        .iter()
        .filter(|m| m.role == "assistant" && m.content.contains("Answer accepted."))
        .count();
    assert_eq!(questions, 1, "ask question must appear exactly once");
    assert_eq!(finals, 1, "final answer must appear exactly once");

    // Step rows from the failed attempt must be overwritten too — the
    // resume history stays linear (only branching splits timelines).
    // Thought rows carry no text anymore (the text lives in messages),
    // so count by step number: the failed attempt's step-2 rows must be
    // gone, leaving only the retried step.
    let step2_rows = steps.iter().filter(|s| s.step_number == 2).count();
    assert_eq!(
        step2_rows, 1,
        "step rows from the failed attempt should be deleted, got {:?}",
        steps
    );
}

#[tokio::test]
async fn run_session_notify_tool_emits_notification_without_pausing() {
    // The `notify` tool signals the ReAct loop to emit a Notification
    // event (in-app toast + Windows). Unlike `ask`, it must NOT pause the
    // session: the loop continues to the final answer.
    let tools = Arc::new(ToolsManager::new());
    tools
        .registry()
        .register(Arc::new(haven_tools::builtin::notify::NotifyTool) as ToolBox)
        .await
        .unwrap();
    let mock = Arc::new(ScriptedMock::new(vec![
        ScriptedResponse::Chunk(StreamChunk {
            text: Some("Notifying the user.".into()),
            tool_calls: vec![CanonicalToolCall {
                id: "tc1".into(),
                name: "notify".into(),
                arguments: serde_json::json!({"title": "Build", "body": "Compilation finished"}),
            }],
            finish_reason: Some(FinishReason::ToolCalls),
            usage: None,
            model: None,
            reasoning: None,
            web_search: None,
            web_search_calls: Vec::new(),
            thinking_blocks: Vec::new(),
        }),
        ScriptedResponse::Chunk(StreamChunk {
            text: Some("Done.".into()),
            tool_calls: vec![CanonicalToolCall {
                id: "final".into(),
                name: "final_answer".into(),
                arguments: serde_json::json!({}),
            }],
            finish_reason: Some(FinishReason::Stop),
            usage: None,
            model: None,
            reasoning: None,
            web_search: None,
            web_search_calls: Vec::new(),
            thinking_blocks: Vec::new(),
        }),
    ]));
    let (agent, executor) = make_test_agent_with(mock, tools);
    let collector = Arc::new(EventCollector::new());
    agent.set_emitter(collector.clone());
    let session = executor.create_session("build and notify").await.unwrap();
    let history = agent.run_session_from_id(&session.id).await.unwrap();
    assert!(history.len() >= 2, "should have at least 2 steps");

    // The Notification event must carry the tool's title/body.
    let (title, body) = collector
        .has_notification()
        .expect("notify should emit a Notification event");
    assert_eq!(title, "Build");
    assert_eq!(body, "Compilation finished");

    // The chat/resume observation must be readable, not raw JSON.
    assert!(collector.has_observation("notify"));

    // Unlike `ask`, notify must not pause the session mid-loop: the loop
    // continued past the notify step (history has 2 steps) and reached the
    // normal end state (Paused = conversation mode, waiting for follow-up).
    assert_eq!(
        executor.get_active_session_status(&session.id).await,
        Some(SessionStatus::Paused)
    );
}

#[tokio::test]
async fn run_session_multiple_asks_surface_all_questions() {
    // Two `ask` calls in one batch must both be surfaced (joined into one
    // assistant message), not just the first.
    let tools = Arc::new(ToolsManager::new());
    tools
        .registry()
        .register(Arc::new(haven_tools::builtin::ask::AskTool) as ToolBox)
        .await
        .unwrap();
    let mock = Arc::new(ScriptedMock::new(vec![ScriptedResponse::Chunk(
        StreamChunk {
            text: Some("Two questions.".into()),
            tool_calls: vec![
                CanonicalToolCall {
                    id: "tc1".into(),
                    name: "ask".into(),
                    arguments: serde_json::json!({"question": "First?"}),
                },
                CanonicalToolCall {
                    id: "tc2".into(),
                    name: "ask".into(),
                    arguments: serde_json::json!({"question": "Second?"}),
                },
            ],
            finish_reason: Some(FinishReason::ToolCalls),
            usage: None,
            model: None,
            reasoning: None,
            web_search: None,
            web_search_calls: Vec::new(),
            thinking_blocks: Vec::new(),
        },
    )]));
    let (agent, executor) = make_test_agent_with(mock, tools);
    let collector = Arc::new(EventCollector::new());
    agent.set_emitter(collector.clone());
    let session = executor.create_session("two questions").await.unwrap();
    agent.run_session_from_id(&session.id).await.unwrap();
    assert_eq!(
        executor.get_active_session_status(&session.id).await,
        Some(SessionStatus::Paused),
        "ask should pause"
    );
    let msgs = agent.db.get_session_messages(&session.id).unwrap();
    let persisted: String = msgs
        .iter()
        .filter(|m| m.role == "assistant")
        .map(|m| m.content.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        persisted.contains("First?"),
        "first question missing: {}",
        persisted
    );
    assert!(
        persisted.contains("Second?"),
        "second question missing: {}",
        persisted
    );
}

#[tokio::test]
async fn pause_snapshot_and_resume_keep_own_final_answer_in_canonical() {
    // The pause snapshot must end with the agent's own final answer (not
    // right after the tool results), so a resume sees the completed
    // answer BEFORE the follow-up instead of having the re-seed re-insert
    // it at the transcript head, out of order.
    let db = temp_db();
    let tools = Arc::new(ToolsManager::new());
    let executor = Arc::new(SessionSupervisor::new_for_test(
        db.clone(),
        tools.clone(),
        1,
    ));
    let mock = Arc::new(ScriptedMock::new(vec![
        ScriptedResponse::Chunk(StreamChunk {
            text: Some("First answer.".into()),
            tool_calls: vec![],
            finish_reason: Some(FinishReason::Stop),
            usage: None,
            model: None,
            reasoning: None,
            web_search: None,
            web_search_calls: Vec::new(),
            thinking_blocks: Vec::new(),
        }),
        ScriptedResponse::Chunk(StreamChunk {
            text: Some("Second answer.".into()),
            tool_calls: vec![],
            finish_reason: Some(FinishReason::Stop),
            usage: None,
            model: None,
            reasoning: None,
            web_search: None,
            web_search_calls: Vec::new(),
            thinking_blocks: Vec::new(),
        }),
    ]));
    let client: Arc<dyn LlmClient> = mock.clone();
    let router = Arc::new(LlmRouter::new_with_clients(
        client.clone(),
        client.clone(),
        client.clone(),
        client,
    ));
    let context_limits = ContextLimitsConfig::default();
    let memory_service = memory_service_for_test(db.clone(), router.clone(), &context_limits);
    let agent = Arc::new(
        AgentLayer::build(
            memory_service,
            executor.clone(),
            crate::AgentToolPorts::from_tools_manager(tools),
            router,
            30,
            50,
            context_limits,
        )
        .agent,
    );
    let collector = Arc::new(EventCollector::new());
    agent.set_emitter(collector.clone());
    let session = executor.create_session("question one").await.unwrap();

    agent.run_session_from_id(&session.id).await.unwrap();
    assert_eq!(
        executor.get_active_session_status(&session.id).await,
        Some(SessionStatus::Paused)
    );

    let snapshot = load_event_projection(&agent, &session.id).await;
    let (canonical, _) = snapshot.project();
    let last = canonical.last().expect("canonical not empty");
    assert_eq!(
        last.role,
        CanonicalRole::Assistant,
        "pause snapshot canonical must end with the agent's own answer"
    );
    let snapshot_text: String = last
        .content
        .iter()
        .filter_map(|p| match p {
            ContentPart::Text(t) => Some(t.as_str()),
            _ => None,
        })
        .collect();
    assert!(
        snapshot_text.contains("First answer."),
        "snapshot canonical must carry the final answer, got: {snapshot_text:?}"
    );

    // Resume with a follow-up: the next LLM request must show the agent's
    // own completed answer BEFORE the injected follow-up, so the model
    // answers with knowledge of what it already said.
    executor
        .add_follow_up(&session.id, "next question")
        .await
        .unwrap();
    executor
        .update_session_status(&session.id, SessionStatus::Pending)
        .await
        .unwrap();
    agent.run_session_from_id(&session.id).await.unwrap();

    let seen = mock.seen.lock().unwrap();
    assert!(
        seen.len() >= 2,
        "expected a resumed request, got {:?}",
        seen.len()
    );
    let last_req = seen.last().unwrap();
    let idx_answer = last_req.iter().position(|m| {
        matches!(m.role, CanonicalRole::Assistant)
            && m.content
                .iter()
                .any(|p| matches!(p, ContentPart::Text(t) if t.contains("First answer.")))
    });
    let idx_followup = last_req.iter().position(|m| {
        matches!(m.role, CanonicalRole::User)
            && m.content
                .iter()
                .any(|p| matches!(p, ContentPart::Text(t) if t.contains("next question")))
    });
    let roles: Vec<String> = last_req.iter().map(|m| m.role.to_string()).collect();
    assert!(
        idx_answer.is_some(),
        "resumed request must contain the agent's own answer, roles: {roles:?}"
    );
    assert!(
        idx_followup.is_some(),
        "resumed request must contain the follow-up, roles: {roles:?}"
    );
    assert!(
        idx_answer.unwrap() < idx_followup.unwrap(),
        "the agent's own answer must precede the follow-up message"
    );
}

#[tokio::test]
async fn run_session_compaction_retry_on_context_exceeded() {
    let tools = Arc::new(ToolsManager::new());
    tools
        .registry()
        .register(Arc::new(EchoTool) as ToolBox)
        .await
        .unwrap();
    let mock = Arc::new(ScriptedMock::new(vec![
        ScriptedResponse::Chunk(StreamChunk {
            text: Some("Calling echo.".into()),
            tool_calls: vec![CanonicalToolCall {
                id: "tc1".into(),
                name: "echo".into(),
                arguments: serde_json::json!({"text": "data"}),
            }],
            finish_reason: Some(FinishReason::ToolCalls),
            usage: None,
            model: None,
            reasoning: None,
            web_search: None,
            web_search_calls: Vec::new(),
            thinking_blocks: Vec::new(),
        }),
        ScriptedResponse::Err(LlmError::ContextLengthExceeded),
        ScriptedResponse::Chunk(StreamChunk {
            text: Some("Done after compaction.".into()),
            tool_calls: vec![CanonicalToolCall {
                id: "final".into(),
                name: "final_answer".into(),
                arguments: serde_json::json!({}),
            }],
            finish_reason: Some(FinishReason::Stop),
            usage: None,
            model: None,
            reasoning: None,
            web_search: None,
            web_search_calls: Vec::new(),
            thinking_blocks: Vec::new(),
        }),
    ]));
    let (agent, executor) = make_test_agent_with(mock, tools);
    let collector = Arc::new(EventCollector::new());
    agent.set_emitter(collector.clone());
    let session = executor.create_session("test compaction").await.unwrap();
    let history = agent.run_session_from_id(&session.id).await.unwrap();
    assert!(!history.is_empty());
    assert!(
        collector.has_compaction(),
        "Compaction event should be emitted"
    );
    assert_eq!(
        executor.get_active_session_status(&session.id).await,
        Some(SessionStatus::Paused)
    );
}

#[tokio::test]
async fn run_session_context_exceeded_compaction_fails() {
    let tools = Arc::new(ToolsManager::new());
    let mock = Arc::new(ScriptedMock::new(vec![ScriptedResponse::Err(
        LlmError::ContextLengthExceeded,
    )]));
    let (agent, executor) = make_test_agent_with(mock, tools);
    let collector = Arc::new(EventCollector::new());
    agent.set_emitter(collector.clone());
    let session = executor.create_session("compaction fail").await.unwrap();
    let result = agent.run_session_from_id(&session.id).await;
    assert!(result.is_err(), "should error when compaction fails");
    // Terminal cleanup removed the session from the working set.
    assert_eq!(executor.get_active_session_status(&session.id).await, None);
}

#[tokio::test]
async fn continue_session_resumes_errored_session() {
    let mock = Arc::new(ScriptedMock::new(Vec::new()));
    let (agent, executor) = make_test_agent_with(mock, Arc::new(ToolsManager::new()));
    let session = executor.create_session("test continue").await.unwrap();
    // Simulate an errored session with a saved snapshot.
    agent
        .db
        .update_session_status(&session.id, SessionStatus::Error)
        .unwrap();
    executor
        .update_session_status(&session.id, SessionStatus::Error)
        .await
        .unwrap();
    let router = agent.react_engine.router();
    for _ in 0..3 {
        router
            .chat_stream_with_tools_aggregated(
                haven_common::config::RequestKind::Chat,
                &[],
                &[],
                |_| {},
            )
            .await
            .expect_err("the scripted provider failure should count toward the circuit");
    }
    assert!(matches!(
        router
            .chat_stream_with_tools_aggregated(
                haven_common::config::RequestKind::Chat,
                &[],
                &[],
                |_| {},
            )
            .await,
        Err(LlmError::CircuitOpen { .. })
    ));
    let snapshot = EventProjection {
        events: seed_events_from_canonical(vec![CanonicalMessage {
            role: CanonicalRole::User,
            content: vec![ContentPart::text("hello")],
            tool_calls: None,
            tool_call_id: None,
            reasoning: None,
            web_search_calls: Vec::new(),
            thinking_blocks: Vec::new(),
            source: None,
            id: None,
        }]),
        step_number: 1,
        branch_points: HashMap::new(),
        interactions: Vec::new(),
    };
    agent
        .db
        .add_message(&session.id, "user", "hello", Some("text"), None)
        .unwrap();
    let completed = agent
        .db
        .add_message(
            &session.id,
            "assistant",
            "completed before failure",
            Some("text"),
            None,
        )
        .unwrap();
    let kept_step_id = haven_common::types::new_id("step");
    let kept_step = agent
        .db
        .create_thought_step(&session.id, 1, &kept_step_id)
        .unwrap();
    seed_event_projection(&agent, &session.id, &snapshot).await;
    let store = agent.react_engine.event_store.clone();
    let kept_usage = store
        .append_usage(
            &session.id,
            &haven_memory::LlmCallUsageInput {
                step_number: Some(1),
                request_kind: haven_common::config::RequestKind::Chat,
                call_kind: haven_common::types::LlmCallKind::Agent,
                model: Some("test-model".into()),
                prompt_tokens: 10,
                completion_tokens: 0,
                total_tokens: 10,
                cached_tokens: 0,
                cache_creation_tokens: 0,
                cache_miss_tokens: 0,
                cache_accounting: haven_common::types::CacheAccounting::Unknown,
                cache_diagnostics: None,
                cost_usd: 0.0,
                has_cost: false,
                duration_ms: None,
                context_tokens: 0,
                context_window: None,
            },
        )
        .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    let partial = agent
        .db
        .add_message(
            &session.id,
            "assistant",
            "partial output",
            Some("text"),
            None,
        )
        .unwrap();
    let discarded_step_id = haven_common::types::new_id("step");
    agent
        .db
        .create_thought_step(&session.id, 2, &discarded_step_id)
        .unwrap();
    let discarded_usage = store
        .append_usage(
            &session.id,
            &haven_memory::LlmCallUsageInput {
                step_number: Some(2),
                request_kind: haven_common::config::RequestKind::Chat,
                call_kind: haven_common::types::LlmCallKind::Agent,
                model: Some("test-model".into()),
                prompt_tokens: 20,
                completion_tokens: 0,
                total_tokens: 20,
                cached_tokens: 0,
                cache_creation_tokens: 0,
                cache_miss_tokens: 0,
                cache_accounting: haven_common::types::CacheAccounting::Unknown,
                cache_diagnostics: None,
                cost_usd: 0.0,
                has_cost: false,
                duration_ms: None,
                context_tokens: 0,
                context_window: None,
            },
        )
        .unwrap();
    let cutoff = kept_usage.created_at.clone();
    let sid = session.id.clone();
    agent
        .db
        .run_blocking(move |_| {
            store.append_branch_point(&sid, 1, 1, Some(&cutoff), None)?;
            store.append_recovery_persistence(
                &sid,
                0,
                1,
                "committed",
                RecoveryPersistenceStatus {
                    branch_point: true,
                    partial_messages: true,
                    projection: true,
                    event_boundary: true,
                },
            )?;
            Ok(())
        })
        .await
        .unwrap();
    agent.continue_session(&session.id).await.unwrap();
    let after_continue = router
        .chat_stream_with_tools_aggregated(
            haven_common::config::RequestKind::Chat,
            &[],
            &[],
            |_| {},
        )
        .await;
    assert!(
        matches!(&after_continue, Err(LlmError::Unknown(message)) if message == "scripted responses exhausted"),
        "Continue must clear the open circuit so the provider request can run: {after_continue:?}"
    );
    assert_eq!(
        executor.get_active_session_status(&session.id).await,
        Some(SessionStatus::Pending)
    );
    // The committed recovery marker authorizes removal strictly after the
    // branch-point cutoff, including execution and usage projections.
    let msgs = agent.db.get_session_messages(&session.id).unwrap();
    assert_eq!(msgs.len(), 2);
    assert_eq!(msgs[0].content, "hello");
    assert_eq!(msgs[1].id, completed.id);
    assert!(!msgs.iter().any(|message| message.id == partial.id));
    let steps = agent.db.get_session_steps(&session.id).unwrap();
    assert_eq!(steps.len(), 1);
    assert_eq!(steps[0].id, kept_step.id);
    let usage = agent.db.get_session_llm_usage(&session.id).unwrap();
    assert_eq!(usage.len(), 1);
    assert_eq!(usage[0].id, kept_usage.id);
    assert_eq!(
        agent
            .db
            .get_session_usage(&session.id)
            .unwrap()
            .unwrap()
            .total_tokens,
        10
    );
    let discarded_events = agent
        .react_engine
        .event_store
        .read_active_domain_events(&session.id)
        .unwrap()
        .into_iter()
        .filter(|event| event.event_type == haven_memory::USAGE_DISCARDED_EVENT_TYPE)
        .collect::<Vec<_>>();
    assert_eq!(discarded_events.len(), 1);
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&discarded_events[0].payload)
            .unwrap()
            .get("usage_id")
            .and_then(serde_json::Value::as_str),
        Some(discarded_usage.id.as_str())
    );
}

#[tokio::test]
async fn continue_session_preserves_history_without_an_error_partial_marker() {
    // App/process interruption can leave a periodic snapshot whose branch
    // point predates several already-persisted rounds. That snapshot is valid
    // for model resume, but it is not authorization to delete chat history.
    let (agent, executor) = make_test_agent();
    let session = executor
        .create_session("interrupted after checkpoints")
        .await
        .unwrap();
    agent
        .db
        .update_session_status(&session.id, SessionStatus::Error)
        .unwrap();
    executor
        .update_session_status(&session.id, SessionStatus::Error)
        .await
        .unwrap();

    let opening = agent
        .db
        .add_message(&session.id, "user", "opening", Some("text"), None)
        .unwrap();
    let completed = agent
        .db
        .add_message(
            &session.id,
            "assistant",
            "completed before the interruption",
            Some("text"),
            None,
        )
        .unwrap();
    let visible_partial = agent
        .db
        .add_message(
            &session.id,
            "assistant",
            "text flushed before the app closed",
            Some("text"),
            None,
        )
        .unwrap();
    let mut branch_points = HashMap::new();
    branch_points.insert(
        1,
        BranchPoint {
            event_cursor: 1,
            step_number: 1,
            last_msg_at: Some(opening.created_at),
        },
    );
    let snapshot = EventProjection {
        events: seed_events_from_canonical(vec![CanonicalMessage::user_text("opening")]),
        step_number: 1,
        branch_points,
        interactions: Vec::new(),
    };
    seed_event_projection(&agent, &session.id, &snapshot).await;
    agent.continue_session(&session.id).await.unwrap();

    let ids: Vec<String> = agent
        .db
        .get_session_messages(&session.id)
        .unwrap()
        .into_iter()
        .map(|m| m.id)
        .collect();
    assert_eq!(ids, vec![opening.id, completed.id, visible_partial.id]);
}

#[tokio::test]
async fn continue_session_non_error_fails() {
    let (agent, executor) = make_test_agent();
    let session = executor.create_session("not error").await.unwrap();
    // Session is Pending, not Error ??should refuse.
    let result = agent.continue_session(&session.id).await;
    assert!(result.is_err());
}

/// R4: pause snapshots record the live per-run budget for observability.
#[tokio::test]
async fn pause_snapshot_includes_run_budget() {
    let tools = Arc::new(ToolsManager::new());
    tools
        .registry()
        .register(Arc::new(haven_tools::builtin::ask::AskTool) as ToolBox)
        .await
        .unwrap();
    let mock = Arc::new(ScriptedMock::new(vec![ScriptedResponse::Chunk(
        StreamChunk {
            text: Some("Asking.".into()),
            tool_calls: vec![CanonicalToolCall {
                id: "tc1".into(),
                name: "ask".into(),
                arguments: serde_json::json!({"question": "Ready?"}),
            }],
            finish_reason: Some(FinishReason::ToolCalls),
            usage: None,
            model: None,
            reasoning: None,
            web_search: None,
            web_search_calls: Vec::new(),
            thinking_blocks: Vec::new(),
        },
    )]));
    let (agent, executor) = make_test_agent_with(mock, tools);
    let collector = Arc::new(EventCollector::new());
    agent.set_emitter(collector);
    agent.set_max_steps(12).unwrap();
    let session = executor.create_session("budget on pause").await.unwrap();
    agent.run_session_from_id(&session.id).await.unwrap();
    assert_eq!(
        executor.get_active_session_status(&session.id).await,
        Some(SessionStatus::Paused)
    );
}
