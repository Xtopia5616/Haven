use super::support::*;
use super::*;

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
        .add_supplement(&session.id, "extra: remember path X")
        .await
        .unwrap();

    let history = agent.run_session_from_id(&session.id).await.unwrap();
    assert!(!history.is_empty());

    let sups = recorder.supplements.lock().unwrap().clone();
    assert_eq!(sups.len(), 1, "exactly one supplement event expected");
    assert_eq!(sups[0], "extra: remember path X");
    // With supplements, session pauses instead of completing (conversation mode)
    let state = executor.get_session_state(&session.id).await;
    assert_eq!(
        state,
        Some(SessionStatus::Paused),
        "session should be paused (not completed) when supplements were processed"
    );
}

#[tokio::test]
async fn loop_pauses_on_pending_ask_instead_of_heuristic_final() {
    // The model responds with text + Stop and no tool calls while an
    // unanswered `ask` is pending: the turn must not end on the
    // synthesized heuristic final ??the loop must pause and wait for
    // the user's answer instead.
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
                r#"{"ask":true,"question":"which file?","awaiting_answer":true,"options":[]}"#,
            )],
            Some("call_ask".into()),
        ),
    ];
    let snapshot = ReActSnapshot {
        events: seed_events_from_canonical(canonical),
        step_number: 1,
        branch_points: HashMap::new(),
        saved_at: None,
        awaiting_answer: None,
        awaiting_confirm: None,
        run_budget: None,
        error_partial_message_ids: None,
        upgrade_tool_rounds: Vec::new(),
    };
    agent
        .db
        .save_react_state(&session.id, &serde_json::to_string(&snapshot).unwrap())
        .unwrap();

    agent.run_session_from_id(&session.id).await.unwrap();

    assert_eq!(
        executor.get_session_state(&session.id).await,
        Some(SessionStatus::PausedAwaitingAnswer),
        "session must pause for the pending question instead of completing"
    );
    assert!(
        executor
            .get_session_state(&session.id)
            .await
            .is_some_and(|s| s.is_awaiting_answer()),
        "pause must be flagged as awaiting the user's answer"
    );
    let msgs = agent.db.get_session_messages(&session.id).unwrap();
    assert_eq!(
        msgs.last().unwrap().content,
        "which file?",
        "the pending question must be surfaced as the pause message"
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
    agent.set_max_steps(1);
    let recorder = make_recording_emitter();
    agent.set_emitter(recorder.clone());
    let session = executor.create_session("session").await.unwrap();
    agent.run_session_from_id(&session.id).await.unwrap();

    assert_eq!(
        executor.get_session_state(&session.id).await,
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
        executor.get_session_state(&session.id).await,
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
async fn run_session_rebuilds_tool_chain_from_steps_without_snapshot() {
    // Phase 7 / B4: when react_state is *missing*, resume uses the
    // shared projector (best-effort). Corrupt snapshots hard-fail
    // instead (see `corrupt_react_state_hard_fails_resume`). The DB
    // message stream holds only text, so the projected canonical must
    // recover tool-call/result pairs from session_steps.
    let tools = Arc::new(ToolsManager::new());
    tools.registry.register(Arc::new(EchoTool) as ToolBox).await;
    let mock = Arc::new(ScriptedMock::new(vec![ScriptedResponse::Chunk(
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
    )]));
    let (agent, executor) = make_test_agent_with(mock.clone(), tools);
    let collector = Arc::new(EventCollector::new());
    agent.set_emitter(collector.clone());
    let session = executor.create_session("resume me").await.unwrap();
    // Persisted text turns (what the DB message stream holds)—
    agent
        .persist_message_parts(&session.id, "user", "resume me", Some("text"), &[], false)
        .await
        .unwrap();
    // …plus the action chain in session_steps (what a snapshot-less resume
    // must reconstruct). Use raw repo calls to avoid going through the
    // ReAct loop.
    agent
        .db
        .run_blocking({
            let session_id = session.id.clone();
            move |db| {
                db.create_thought_step(&session_id, 1, "step-echo-thought")?;
                let step = db.create_action_step(
                    &session_id,
                    2,
                    "echo",
                    r#"{"text":"hi"}"#,
                    false,
                    false,
                    None,
                    None,
                )?;
                db.complete_action_step(&step.id, "hi", true)?;
                Ok::<(), anyhow::Error>(())
            }
        })
        .await
        .unwrap();
    // NO react_state row: fallback path.
    assert!(agent.db.get_react_state(&session.id).unwrap().is_none());

    agent.run_session_from_id(&session.id).await.unwrap();

    {
        let seen = mock.seen.lock().unwrap();
        assert_eq!(seen.len(), 1, "fresh run after snapshot-less resume");
        let first = &seen[0];
        let roles: Vec<String> = first.iter().map(|m| m.role.to_string()).collect();
        // The rebuilt chain must appear: assistant with tool_calls,
        // followed by its tool result (sanitize may keep them intact).
        assert!(
            roles.iter().any(|r| r == "assistant"),
            "expected an assistant tool-call message: {:?}",
            roles
        );
        let rebuilt_tool = first.iter().any(|m| {
            matches!(m.role, CanonicalRole::Assistant)
                && m.tool_calls.as_ref().is_some_and(|c| {
                    c.iter()
                        .any(|tc| tc.name == "echo" && tc.id.starts_with("call-"))
                })
        });
        assert!(
            rebuilt_tool,
            "snapshot-less resume must rebuild the echo call from session_steps"
        );
        let rebuilt_result = first.iter().any(|m| {
            matches!(m.role, CanonicalRole::Tool)
                && m.tool_call_id
                    .as_deref()
                    .is_some_and(|id| id.starts_with("call-"))
        });
        assert!(
            rebuilt_result,
            "snapshot-less resume must rebuild the echo result from session_steps"
        );
    }
}

#[tokio::test]
async fn run_session_executes_tool_then_final_answer() {
    let tools = Arc::new(ToolsManager::new());
    tools.registry.register(Arc::new(EchoTool) as ToolBox).await;
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
        executor.get_session_state(&session.id).await,
        Some(SessionStatus::Paused)
    );
}

#[tokio::test]
async fn run_session_empty_tool_call_id_stays_consistent_in_canonical() {
    // Some providers return an empty tool_call_id. The Action side
    // synthesizes a UUID; the canonical assistant declaration must echo
    // the SAME id (not the raw empty string), otherwise the tool result
    // references an id the assistant never declared and the next request
    // is rejected with a 400.
    let tools = Arc::new(ToolsManager::new());
    tools.registry.register(Arc::new(EchoTool) as ToolBox).await;
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
    let saved: ReActSnapshot =
        serde_json::from_str(&agent.db.get_react_state(&session.id).unwrap().unwrap()).unwrap();
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
    tools.registry.register(Arc::new(EchoTool) as ToolBox).await;
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
        executor.get_session_state(&session.id).await,
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
        .registry
        .register(Arc::new(TimingTool::new("delay_a", timing.clone())) as ToolBox)
        .await;
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
        executor.get_session_state(&session.id).await,
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
        .registry
        .register(Arc::new(haven_tools::builtin::ask::AskTool) as ToolBox)
        .await;
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
        executor.get_session_state(&session.id).await,
        Some(SessionStatus::PausedAwaitingAnswer),
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
}

#[tokio::test]
async fn run_session_ask_resumes_after_user_answer() {
    // After `ask` pauses the session, the user's reply arrives as a
    // supplement; the loop resumes and should reach final_answer.
    let tools = Arc::new(ToolsManager::new());
    tools
        .registry
        .register(Arc::new(haven_tools::builtin::ask::AskTool) as ToolBox)
        .await;
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
        executor.get_session_state(&session.id).await,
        Some(SessionStatus::PausedAwaitingAnswer),
        "ask should pause"
    );

    // User answers; the supplement flips the session back to Pending.
    executor
        .add_supplement(&session.id, "Use option A.")
        .await
        .unwrap();
    executor
        .update_session_status(&session.id, SessionStatus::Pending)
        .await
        .unwrap();
    agent.run_session_from_id(&session.id).await.unwrap();
    assert_eq!(
        executor.get_session_state(&session.id).await,
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
        .registry
        .register(Arc::new(haven_tools::builtin::ask::AskTool) as ToolBox)
        .await;
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
        executor.get_session_state(&session.id).await,
        Some(SessionStatus::PausedAwaitingAnswer)
    );

    // Turn 2: the user answers; the resumed step fails mid-stream.
    executor.add_supplement(&session.id, "Yes").await.unwrap();
    executor
        .update_session_status(&session.id, SessionStatus::Pending)
        .await
        .unwrap();
    let _ = agent.run_session_from_id(&session.id).await;
    // The failed run ended in Error; terminal cleanup removed the session
    // from the working set.
    assert_eq!(executor.get_session_state(&session.id).await, None);

    // Turn 3: retry via continue_session ??Pending ??re-run.
    agent.continue_session(&session.id).await.unwrap();
    assert_eq!(
        executor.get_session_state(&session.id).await,
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
        .registry
        .register(Arc::new(haven_tools::builtin::notify::NotifyTool) as ToolBox)
        .await;
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
        executor.get_session_state(&session.id).await,
        Some(SessionStatus::Paused)
    );
}

#[tokio::test]
async fn run_session_multiple_asks_surface_all_questions() {
    // Two `ask` calls in one batch must both be surfaced (joined into one
    // assistant message), not just the first.
    let tools = Arc::new(ToolsManager::new());
    tools
        .registry
        .register(Arc::new(haven_tools::builtin::ask::AskTool) as ToolBox)
        .await;
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
        executor.get_session_state(&session.id).await,
        Some(SessionStatus::PausedAwaitingAnswer),
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
    let executor = Arc::new(SessionExecutor::new(db.clone(), tools, 1));
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
        client.clone(),
        client,
    ));
    let agent = Arc::new(AgentLayer::new(
        db.clone(),
        executor.clone(),
        router,
        30,
        50,
        ContextLimitsConfig::default(),
    ));
    let collector = Arc::new(EventCollector::new());
    agent.set_emitter(collector.clone());
    let session = executor.create_session("question one").await.unwrap();

    agent.run_session_from_id(&session.id).await.unwrap();
    assert_eq!(
        executor.get_session_state(&session.id).await,
        Some(SessionStatus::Paused)
    );

    let state_json = db
        .get_react_state(&session.id)
        .unwrap()
        .expect("snapshot must exist after the pause");
    let snapshot: ReActSnapshot = serde_json::from_str(&state_json).unwrap();
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
        .add_supplement(&session.id, "next question")
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
    tools.registry.register(Arc::new(EchoTool) as ToolBox).await;
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
        executor.get_session_state(&session.id).await,
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
    assert_eq!(executor.get_session_state(&session.id).await, None);
}

#[tokio::test]
async fn continue_session_resumes_errored_session() {
    let (agent, executor) = make_test_agent();
    let session = executor.create_session("test continue").await.unwrap();
    // Simulate an errored session with a saved snapshot.
    agent
        .db
        .update_session_status(&session.id, "error")
        .unwrap();
    executor
        .update_session_status(&session.id, SessionStatus::Error)
        .await
        .unwrap();
    let mut snapshot = ReActSnapshot {
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
        saved_at: None,
        awaiting_answer: None,
        awaiting_confirm: None,
        run_budget: None,
        error_partial_message_ids: None,
        upgrade_tool_rounds: Vec::new(),
    };
    // Add a partial assistant message that should be cleaned up.
    agent
        .db
        .add_message(&session.id, "user", "hello", Some("text"), None)
        .unwrap();
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
    snapshot.error_partial_message_ids = Some(vec![partial.id]);
    agent
        .db
        .save_react_state(&session.id, &serde_json::to_string(&snapshot).unwrap())
        .unwrap();

    agent.continue_session(&session.id).await.unwrap();
    assert_eq!(
        executor.get_session_state(&session.id).await,
        Some(SessionStatus::Pending)
    );
    // The explicitly marked partial output should have been deleted.
    let msgs = agent.db.get_session_messages(&session.id).unwrap();
    assert_eq!(msgs.len(), 1);
    assert_eq!(msgs[0].content, "hello");
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
        .update_session_status(&session.id, "error")
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
    let snapshot = ReActSnapshot {
        events: seed_events_from_canonical(vec![CanonicalMessage::user_text("opening")]),
        step_number: 1,
        branch_points,
        saved_at: None,
        error_partial_message_ids: None,
        awaiting_answer: None,
        awaiting_confirm: None,
        run_budget: None,
        upgrade_tool_rounds: Vec::new(),
    };
    agent
        .db
        .save_react_state(&session.id, &serde_json::to_string(&snapshot).unwrap())
        .unwrap();

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
        .registry
        .register(Arc::new(haven_tools::builtin::ask::AskTool) as ToolBox)
        .await;
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
    agent.set_max_steps(12);
    let session = executor.create_session("budget on pause").await.unwrap();
    agent.run_session_from_id(&session.id).await.unwrap();
    assert_eq!(
        executor.get_session_state(&session.id).await,
        Some(SessionStatus::PausedAwaitingAnswer)
    );
    let snap =
        ReActSnapshot::from_json(&agent.db.get_react_state(&session.id).unwrap().unwrap()).unwrap();
    let budget = snap.run_budget.expect("run_budget written on pause");
    assert_eq!(budget.max_steps, 12);
    assert!(budget.effective_max >= budget.start_step);
    assert_eq!(budget.start_step, 1);
}
