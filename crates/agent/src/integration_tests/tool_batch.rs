use super::support::*;
use super::*;

#[tokio::test]
async fn run_session_parallel_tool_execution() {
    let tools = Arc::new(ToolsManager::new());
    let timing = Arc::new(TimingState::new());
    tools
        .registry()
        .register(Arc::new(TimingTool::new("delay_a", timing.clone())) as ToolBox)
        .await
        .unwrap();
    tools
        .registry()
        .register(Arc::new(TimingTool::new("delay_b", timing.clone())) as ToolBox)
        .await
        .unwrap();
    let mock = Arc::new(ScriptedMock::new(vec![
        ScriptedResponse::Chunk(StreamChunk {
            text: Some("Running both in parallel.".into()),
            tool_calls: vec![
                CanonicalToolCall {
                    id: "tc1".into(),
                    name: "delay_a".into(),
                    arguments: serde_json::json!({}),
                },
                CanonicalToolCall {
                    id: "tc2".into(),
                    name: "delay_b".into(),
                    arguments: serde_json::json!({}),
                },
            ],
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
    let session = executor.create_session("run parallel").await.unwrap();
    let history = agent.run_session_from_id(&session.id).await.unwrap();
    assert!(!history.is_empty());
    assert!(collector.has_action("delay_a"));
    assert!(collector.has_action("delay_b"));
    let step1 = history
        .iter()
        .find(|r| r.step_number == 1)
        .expect("step 1 round");
    assert_eq!(
        step1.tools.len(),
        2,
        "parallel tools must be siblings on one ReActRound (not N fake steps)"
    );
    let mut intervals = timing.intervals.lock().unwrap().clone();
    assert_eq!(intervals.len(), 2, "both tools should have executed");
    intervals.sort_by_key(|(start, _)| *start);
    let (_, a_end) = intervals[0];
    let (b_start, _) = intervals[1];
    assert!(
        b_start < a_end,
        "tools should execute in parallel (overlap)"
    );
}

#[tokio::test]
async fn run_session_contains_custom_extension_panic() {
    let names = ["custom_panic"];
    let tools = Arc::new(ToolsManager::new());
    for name in names {
        tools
            .registry()
            .register(Arc::new(PanicTool {
                tool_name: name.into(),
            }) as ToolBox)
            .await
            .unwrap();
    }

    let calls = names
        .iter()
        .enumerate()
        .map(|(index, name)| CanonicalToolCall {
            id: format!("panic-{index}"),
            name: (*name).into(),
            arguments: serde_json::json!({}),
        })
        .collect();
    let mock = Arc::new(ScriptedMock::new(vec![
        ScriptedResponse::Chunk(StreamChunk {
            text: Some("Running extension checks.".into()),
            tool_calls: calls,
            finish_reason: Some(FinishReason::ToolCalls),
            usage: None,
            model: None,
            reasoning: None,
            web_search: None,
            web_search_calls: Vec::new(),
            thinking_blocks: Vec::new(),
        }),
        ScriptedResponse::Chunk(StreamChunk {
            text: Some("Recovered after extension failures.".into()),
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
    let session = executor
        .create_session("extension panic boundary")
        .await
        .unwrap();

    let history = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        agent.run_session_from_id(&session.id),
    )
    .await
    .expect("extension panic session must not hang")
    .unwrap();
    assert!(!history.is_empty(), "the session must recover and continue");
    assert!(collector.has_action("custom_panic"));
    assert!(collector.has_observation("custom_panic"));
    let events = collector.events.lock().unwrap();
    let panic_observations = events
        .iter()
        .filter(|event| {
            matches!(
                event,
                AgentEvent::Observation { observation, .. }
                    if observation.contains("panicked during execution")
            )
        })
        .count();
    assert_eq!(panic_observations, 1);
}

#[tokio::test]
async fn run_session_contains_real_mcp_and_skill_adapter_panics() {
    let tools = Arc::new(ToolsManager::new());
    tools
        .authorization()
        .set_permission_mode(haven_common::types::PermissionMode::Autonomous)
        .await;
    tools
        .authorization()
        .set_boundaries(
            haven_common::types::SandboxMode::FullAccess,
            Vec::new(),
            haven_common::types::NetworkPolicy::Open,
        )
        .await;
    let mcp_client = Arc::new(haven_tools::McpClient::new(
        &haven_common::McpServerConfig {
            name: "panic-server".into(),
            ..Default::default()
        },
        2 * 1024 * 1024,
        2 * 1024 * 1024,
    ));
    let mcp_adapter = haven_tools::McpToolAdapter::new_panicking_for_test(
        mcp_client,
        "panic-server",
        haven_tools::McpToolInfo {
            name: "panic_tool".into(),
            description: "panics for adapter-boundary testing".into(),
            input_schema: serde_json::json!({"type": "object"}),
        },
    );
    tools
        .registry()
        .register(Arc::new(mcp_adapter) as ToolBox)
        .await
        .unwrap();

    let skill = Arc::new(haven_tools::Skill::from_manifest_unchecked(
        haven_tools::SkillManifest {
            name: "panic-skill".into(),
            description: "panics for adapter-boundary testing".into(),
            version: None,
            language: haven_tools::Language::Python,
            instructions: String::new(),
        },
        std::path::PathBuf::from("."),
        true,
    ));
    let skill_config = haven_common::config::SkillsExecConfig::default();
    let skill_runner = haven_tools::SkillRunner::new(
        haven_tools::VenvManager::new(skill_config.venv_root.clone()),
        skill_config,
    );
    let skill_adapter = haven_tools::SkillToolAdapter::new_panicking_for_test(skill, skill_runner);
    tools
        .registry()
        .register(Arc::new(skill_adapter) as ToolBox)
        .await
        .unwrap();

    let mcp_name = "mcp__panic-server__panic_tool";
    let skill_name = "skill__panic-skill";
    for (tool_name, input) in [
        (mcp_name, serde_json::json!({})),
        (skill_name, serde_json::json!({"params": {}})),
    ] {
        let policy = tools.get_operation_policy(None, tool_name, &input).await;
        tools
            .authorization()
            .grant(
                None,
                policy.capability.to_string(),
                haven_common::types::PermissionEffect::Allow,
                haven_common::types::PermissionScope::Always,
            )
            .await;
    }
    let mock = Arc::new(ScriptedMock::new(vec![
        ScriptedResponse::Chunk(StreamChunk {
            text: Some("Running adapter checks.".into()),
            tool_calls: vec![
                CanonicalToolCall {
                    id: "mcp-panic".into(),
                    name: mcp_name.into(),
                    arguments: serde_json::json!({}),
                },
                CanonicalToolCall {
                    id: "skill-panic".into(),
                    name: skill_name.into(),
                    arguments: serde_json::json!({"params": {}}),
                },
            ],
            finish_reason: Some(FinishReason::ToolCalls),
            usage: None,
            model: None,
            reasoning: None,
            web_search: None,
            web_search_calls: Vec::new(),
            thinking_blocks: Vec::new(),
        }),
        ScriptedResponse::Chunk(StreamChunk {
            text: Some("Recovered after adapter failures.".into()),
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
    let session = executor
        .create_session("real adapter panic boundary")
        .await
        .unwrap();

    let history = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        agent.run_session_from_id(&session.id),
    )
    .await
    .expect("real adapter panic session must not hang")
    .unwrap();
    assert!(!history.is_empty(), "the session must recover and continue");
    assert!(collector.has_action(mcp_name));
    assert!(collector.has_action(skill_name));
    let events = collector.events.lock().unwrap();
    let panic_observations = events
        .iter()
        .filter(|event| {
            matches!(
                event,
                AgentEvent::Observation { observation, .. }
                    if observation.contains("panicked during execution")
            )
        })
        .count();
    assert_eq!(panic_observations, 2);
}

#[tokio::test]
async fn run_session_cancelled_mid_batch_surfaces_interrupted_tools() {
    // A tool batch cancelled mid-flight must NOT silently drop the
    // in-flight calls: each one is repaired with an "Interrupted"
    // observation (so the UI shows it and the model can retry) and the
    // snapshot canonical stays a valid assistant/tool chain.
    let tools = Arc::new(ToolsManager::new());
    let timing = Arc::new(TimingState::new());
    tools
        .registry()
        .register(Arc::new(TimingTool::new("delay_a", timing.clone())) as ToolBox)
        .await
        .unwrap();
    tools
        .registry()
        .register(Arc::new(TimingTool::new("delay_b", timing.clone())) as ToolBox)
        .await
        .unwrap();
    let mock = Arc::new(ScriptedMock::new(vec![ScriptedResponse::Chunk(
        StreamChunk {
            text: Some("Running both in parallel.".into()),
            tool_calls: vec![
                CanonicalToolCall {
                    id: "tc1".into(),
                    name: "delay_a".into(),
                    arguments: serde_json::json!({}),
                },
                CanonicalToolCall {
                    id: "tc2".into(),
                    name: "delay_b".into(),
                    arguments: serde_json::json!({}),
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
    let (agent, executor) = make_test_agent_with(mock.clone(), tools);
    let collector = Arc::new(EventCollector::new());
    agent.set_emitter(collector.clone());
    let session = executor.create_session("run parallel").await.unwrap();

    let run = tokio::spawn({
        let agent = agent.clone();
        let session_id = session.id.clone();
        async move { agent.run_session_from_id(&session_id).await }
    });
    // Wait until both action events and both tool executions are visible,
    // then cancel while both tools (200ms sleeps) are still in flight.
    for _ in 0..50 {
        if collector.has_action("delay_a")
            && collector.has_action("delay_b")
            && timing.started.load(std::sync::atomic::Ordering::Acquire) == 2
        {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert!(
        collector.has_action("delay_a") && collector.has_action("delay_b"),
        "batch must have started before the cancel"
    );
    assert_eq!(
        timing.started.load(std::sync::atomic::Ordering::Acquire),
        2,
        "both tools must have entered execution before the cancel"
    );
    // end_session registers a real cancellation token (entry().or_insert) and
    // cancels it — the same path the frontend's "end session" button uses —
    // so the in-flight tool batch observes the cancellation mid-drain.
    executor.end_session(&session.id).await.unwrap();
    let history = run.await.unwrap().unwrap();

    // Every in-flight tool got an "Interrupted" observation emitted to
    // the UI — the cancelled tools are not silently skipped.
    let interrupted = collector.interrupted_observations();
    assert_eq!(
        interrupted.len(),
        2,
        "both in-flight tools must emit an Interrupted observation"
    );
    // The observation must carry the tool name and the attempted arguments
    // (field supplementation), not a bare "Interrupted" marker, so the UI
    // and the model can see exactly which call was cut off.
    let all_text = interrupted.join("\n");
    for tool in ["delay_a", "delay_b"] {
        assert!(
            all_text.contains(tool),
            "interrupted observation must name tool '{}' (got: {})",
            tool,
            all_text
        );
    }
    assert!(
        all_text.contains("arguments"),
        "interrupted observation must carry the attempted arguments (got: {})",
        all_text
    );
    // The rounds recorded the interrupted tools so a resume keeps them.
    let interrupted_tools: Vec<&str> = history
        .iter()
        .flat_map(|r| r.tools.iter())
        .filter_map(|t| t.observation.as_deref())
        .filter(|o| o.contains("Interrupted"))
        .collect();
    assert_eq!(
        interrupted_tools.len(),
        2,
        "interrupted tool calls must be recorded in rounds"
    );
    // The tool observations must also carry the enriched fields.
    assert!(
        interrupted_tools
            .iter()
            .all(|o| o.contains("tool:") && o.contains("arguments")),
        "interrupted history observations must carry tool name and arguments"
    );
    // The saved snapshot canonical stays a valid assistant/tool chain: no
    // dangling assistant tool_calls without a following result (which
    // providers would reject as a 400 on resume).
    let state_json = agent
        .db
        .get_react_state(&session.id)
        .unwrap()
        .expect("exit snapshot must be saved after mid-batch cancel");
    let snapshot: ReActSnapshot = serde_json::from_str(&state_json).unwrap();
    let mut pending: Vec<String> = Vec::new();
    let mut interrupted_with_fields = 0;
    let (canonical, _) = snapshot.project();
    for m in &canonical {
        match m.role {
            CanonicalRole::Tool => {
                if let Some(cid) = &m.tool_call_id {
                    if let Some(pos) = pending.iter().position(|p| p == cid) {
                        pending.remove(pos);
                    }
                } else if let Some(cid) = pending.pop() {
                    let _ = cid;
                }
                // The repaired Interrupted tool results must carry the
                // tool name and arguments so a resume sees what happened.
                if m.content
                    .iter()
                    .any(|p| matches!(p, ContentPart::Text(t) if t.contains("Interrupted")))
                {
                    interrupted_with_fields += 1;
                    assert!(
                        m.content.iter().any(|p| matches!(
                            p,
                            ContentPart::Text(t) if t.contains("tool:") && t.contains("arguments")
                        )),
                        "repaired Interrupted result must include tool name and arguments: {:?}",
                        m.content
                    );
                }
            }
            CanonicalRole::Assistant => {
                pending = m
                    .tool_calls
                    .as_ref()
                    .map(|tc| tc.iter().map(|t| t.id.clone()).collect())
                    .unwrap_or_default();
            }
            _ => {}
        }
    }
    assert_eq!(
        interrupted_with_fields, 2,
        "both interrupted tool results in the snapshot must carry fields"
    );
    assert!(
        pending.is_empty(),
        "snapshot canonical must not end with unanswered tool_calls (got {:?})",
        pending
    );
    // Pending step rows created at Action emit must be completed with the
    // Interrupted observation so resume rebuilds the tool cards
    // from session_steps (not live-only UI state).
    let db_steps = agent.db.get_session_steps(&session.id).unwrap();
    let interrupted_db = db_steps
        .iter()
        .filter(|s| {
            s.action_tool.is_some()
                && s.observation
                    .as_deref()
                    .is_some_and(|o| o.contains("Interrupted"))
        })
        .count();
    assert_eq!(
        interrupted_db,
        2,
        "interrupted tools must be persisted in session_steps for UI rebuild (got {:?})",
        db_steps
            .iter()
            .map(|s| (
                s.action_tool.clone(),
                s.status.clone(),
                s.observation.clone()
            ))
            .collect::<Vec<_>>()
    );
}
