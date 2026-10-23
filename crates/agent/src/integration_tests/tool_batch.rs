use super::support::*;
use super::*;

struct DeadlineTool {
    tool_name: &'static str,
    executions: Arc<std::sync::atomic::AtomicUsize>,
    started: Arc<tokio::sync::Notify>,
    hold_open: bool,
}

#[async_trait]
impl Tool for DeadlineTool {
    fn name(&self) -> String {
        self.tool_name.into()
    }

    fn description(&self) -> String {
        "Controls a side effect around a turn deadline".into()
    }

    fn risk_level(&self, _: &serde_json::Value) -> RiskLevel {
        RiskLevel::Safe
    }

    fn concurrency(&self, _: &serde_json::Value) -> ToolConcurrency {
        ToolConcurrency::ReadOnly
    }

    fn idempotency(&self, _: &serde_json::Value) -> haven_common::tools::OperationIdempotency {
        haven_common::tools::OperationIdempotency::NonIdempotent
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({"type": "object"})
    }

    async fn execute(
        &self,
        _: serde_json::Value,
        _: CancellationToken,
    ) -> anyhow::Result<ToolResult> {
        self.executions
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.started.notify_one();
        if self.hold_open {
            std::future::pending::<()>().await;
        }
        Ok(ToolResult::ok(serde_json::json!({"executed": true})))
    }
}

fn tool_batch_response(calls: Vec<CanonicalToolCall>) -> ScriptedResponse {
    ScriptedResponse::Chunk(StreamChunk {
        tool_call_updates: Vec::new(),
        text: Some("Run the requested tools.".into()),
        tool_calls: calls,
        finish_reason: Some(FinishReason::ToolCalls),
        usage: None,
        model: None,
        reasoning: None,
        web_search: None,
        web_search_calls: Vec::new(),
        thinking_blocks: Vec::new(),
    })
}

fn final_answer_response() -> ScriptedResponse {
    ScriptedResponse::Chunk(StreamChunk {
        tool_call_updates: Vec::new(),
        text: Some("The operation was interrupted; I will not repeat it.".into()),
        tool_calls: vec![CanonicalToolCall {
            tool_call_id: "final".into(),
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
    })
}

fn assert_tool_calls_have_ordered_results(messages: &[CanonicalMessage], expected: &[&str]) {
    let calls = messages
        .iter()
        .filter(|message| message.role == CanonicalRole::Assistant)
        .flat_map(|message| {
            message
                .tool_calls
                .as_deref()
                .unwrap_or_default()
                .iter()
                .map(|call| call.tool_call_id.as_str())
        })
        .collect::<Vec<_>>();
    let results = messages
        .iter()
        .filter(|message| message.role == CanonicalRole::Tool)
        .filter_map(|message| message.tool_call_id.as_deref())
        .collect::<Vec<_>>();
    assert_eq!(calls, expected, "the declared calls must retain plan order");
    assert_eq!(
        results, expected,
        "each result must match its declared call"
    );
}

struct ConfirmationBatchCounterTool {
    tool_name: &'static str,
    risk: RiskLevel,
    executions: Arc<std::sync::atomic::AtomicUsize>,
}

#[async_trait]
impl Tool for ConfirmationBatchCounterTool {
    fn name(&self) -> String {
        self.tool_name.into()
    }

    fn description(&self) -> String {
        "Counts confirmation batch test executions".into()
    }

    fn risk_level(&self, _: &serde_json::Value) -> RiskLevel {
        self.risk
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({"type": "object"})
    }

    async fn execute(
        &self,
        _: serde_json::Value,
        _: CancellationToken,
    ) -> anyhow::Result<ToolResult> {
        self.executions
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(ToolResult::ok(serde_json::json!({"executed": true})))
    }
}

#[tokio::test]
async fn mixed_confirmation_batch_pauses_only_real_gates_and_resumes_all_tools() {
    let tools = Arc::new(ToolsFacade::new());
    let safe_executions = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let gated_executions = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    for (tool_name, risk, executions) in [
        ("batch_safe", RiskLevel::Safe, safe_executions.clone()),
        ("batch_gated", RiskLevel::High, gated_executions.clone()),
    ] {
        tools
            .share_services()
            .tool_catalog_test_support
            .register_installed_tool(Arc::new(ConfirmationBatchCounterTool {
                tool_name,
                risk,
                executions,
            }) as ToolHandle)
            .await
            .unwrap();
    }
    let mock = Arc::new(ScriptedMock::new(vec![
        ScriptedResponse::Chunk(StreamChunk {
            tool_call_updates: Vec::new(),
            text: Some("Run both operations after confirmation.".into()),
            tool_calls: vec![
                CanonicalToolCall {
                    tool_call_id: "safe-call".into(),
                    name: "batch_safe".into(),
                    arguments: serde_json::json!({}),
                },
                CanonicalToolCall {
                    tool_call_id: "gated-call".into(),
                    name: "batch_gated".into(),
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
            tool_call_updates: Vec::new(),
            text: Some("Both operations completed.".into()),
            tool_calls: vec![CanonicalToolCall {
                tool_call_id: "final".into(),
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
    let (agent, executor) = make_test_agent_with(mock, tools.clone());
    tools
        .share_services()
        .authorization
        .set_execution_mode(haven_common::types::ExecutionMode::Collaborative)
        .await;
    let gated_authorization = tools
        .resolve_authorization_request(
            Some("ses-9930b20bf67b3ba14286afbbdd24fd6a"),
            "batch_gated",
            &serde_json::json!({}),
        )
        .await
        .unwrap();
    assert_eq!(gated_authorization.policy.risk_level, RiskLevel::High);
    assert!(matches!(
        tools
            .share_services()
            .authorization
            .authorize(&gated_authorization)
            .await,
        haven_tools::AuthorizationDecision::RequiresConfirmation { .. }
    ));
    let session = executor
        .create_session("mixed confirmation batch")
        .await
        .unwrap();
    let collector = Arc::new(EventCollector::new());
    agent.set_emitter(collector.clone());

    let first_history = agent
        .run_session_from_id(&session.session_id)
        .await
        .unwrap();

    let confirmations = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            let requests = executor
                .pending_interactions(
                    &session.session_id,
                    crate::interaction::InteractionKind::Confirm,
                )
                .await;
            if !requests.is_empty() {
                break requests;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the real gated operation should register a pending confirmation");
    let status = executor
        .get_active_session_status(&session.session_id)
        .await;
    let event_types = agent
        .react_engine
        .event_store
        .read_active_domain_events(&session.session_id)
        .unwrap()
        .into_iter()
        .map(|event| event.event_type)
        .collect::<Vec<_>>();
    assert_eq!(
        status,
        Some(haven_common::lifecycle::SessionStatus::Paused),
        "the high risk sibling should hold the mixed batch at confirmation; rounds={}, ui_events={}, requests={}, safe_runs={}, gated_runs={}, events={event_types:?}",
        first_history.len(),
        collector.events.lock().unwrap().len(),
        confirmations.len(),
        safe_executions.load(std::sync::atomic::Ordering::SeqCst),
        gated_executions.load(std::sync::atomic::Ordering::SeqCst),
    );
    assert_eq!(
        confirmations.len(),
        1,
        "only the gated operation is an owner request"
    );
    assert_eq!(safe_executions.load(std::sync::atomic::Ordering::SeqCst), 0);
    assert_eq!(
        gated_executions.load(std::sync::atomic::Ordering::SeqCst),
        0
    );
    executor
        .resolve_interaction(
            &session.session_id,
            &confirmations[0].request_id,
            serde_json::Value::Bool(true),
            false,
        )
        .await
        .unwrap()
        .expect("the real confirmation request should resolve");

    agent
        .run_session_from_id(&session.session_id)
        .await
        .unwrap();

    assert_eq!(safe_executions.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert_eq!(
        gated_executions.load(std::sync::atomic::Ordering::SeqCst),
        1
    );
    assert!(
        executor
            .interaction_requests(&session.session_id)
            .await
            .is_empty()
    );
    let events = agent
        .react_engine
        .event_store
        .read_active_domain_events(&session.session_id)
        .unwrap();
    assert!(
        events
            .iter()
            .any(|event| event.event_type == haven_memory::CONFIRMATION_BATCH_PLANNED_EVENT_TYPE)
    );
    assert!(
        events
            .iter()
            .any(|event| event.event_type == haven_memory::INTERACTION_CLEARED_EVENT_TYPE)
    );
}

#[tokio::test]
async fn run_session_parallel_tool_execution() {
    let tools = Arc::new(ToolsFacade::new());
    let timing = Arc::new(TimingState::new());
    tools
        .share_services()
        .tool_catalog_test_support
        .register_installed_tool(Arc::new(TimingTool::new("delay_a", timing.clone())) as ToolHandle)
        .await
        .unwrap();
    tools
        .share_services()
        .tool_catalog_test_support
        .register_installed_tool(Arc::new(TimingTool::new("delay_b", timing.clone())) as ToolHandle)
        .await
        .unwrap();
    let mock = Arc::new(ScriptedMock::new(vec![
        ScriptedResponse::Chunk(StreamChunk {
            tool_call_updates: Vec::new(),
            text: Some("Running both in parallel.".into()),
            tool_calls: vec![
                CanonicalToolCall {
                    tool_call_id: "tc1".into(),
                    name: "delay_a".into(),
                    arguments: serde_json::json!({}),
                },
                CanonicalToolCall {
                    tool_call_id: "tc2".into(),
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
            tool_call_updates: Vec::new(),
            text: Some("Done.".into()),
            tool_calls: vec![CanonicalToolCall {
                tool_call_id: "final".into(),
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
    let history = agent
        .run_session_from_id(&session.session_id)
        .await
        .unwrap();
    assert!(!history.is_empty());
    assert!(collector.has_tool_call("delay_a"));
    assert!(collector.has_tool_call("delay_b"));
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
async fn parallel_tool_result_is_published_before_a_slow_sibling_finishes() {
    let tools = Arc::new(ToolsFacade::new());
    let timing = Arc::new(TimingState::new());
    tools
        .share_services()
        .tool_catalog_test_support
        .register_installed_tool(Arc::new(TimingTool::with_delay(
            "delay_fast",
            timing.clone(),
            std::time::Duration::from_millis(10),
        )) as ToolHandle)
        .await
        .unwrap();
    tools
        .share_services()
        .tool_catalog_test_support
        .register_installed_tool(Arc::new(TimingTool::with_delay(
            "delay_slow",
            timing,
            std::time::Duration::from_secs(3),
        )) as ToolHandle)
        .await
        .unwrap();
    let mock = Arc::new(ScriptedMock::new(vec![
        ScriptedResponse::Chunk(StreamChunk {
            tool_call_updates: Vec::new(),
            text: Some("Run the fast and slow tools.".into()),
            tool_calls: vec![
                CanonicalToolCall {
                    tool_call_id: "fast-call".into(),
                    name: "delay_fast".into(),
                    arguments: serde_json::json!({}),
                },
                CanonicalToolCall {
                    tool_call_id: "slow-call".into(),
                    name: "delay_slow".into(),
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
            tool_call_updates: Vec::new(),
            text: Some("Both tools finished.".into()),
            tool_calls: vec![CanonicalToolCall {
                tool_call_id: "final".into(),
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
        .create_session("show completed parallel results")
        .await
        .unwrap();
    let run = {
        let agent = agent.clone();
        let session_id = session.session_id.clone();
        tokio::spawn(async move { agent.run_session_from_id(&session_id).await })
    };

    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        while !collector.has_observation("delay_fast") {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the fast tool result should publish while its sibling is still running");
    assert!(
        !run.is_finished(),
        "the slow sibling must still hold the batch open"
    );
    assert!(!collector.has_observation("delay_slow"));
    {
        let events = collector.events.lock().unwrap();
        let thought_index = events
            .iter()
            .position(|event| {
                matches!(
                    event,
                    AgentEvent::Thought { thought, .. }
                        if thought == "Run the fast and slow tools."
                )
            })
            .expect("the complete assistant preamble should publish before tool execution");
        let first_tool_index = events
            .iter()
            .position(|event| matches!(event, AgentEvent::ToolCall { .. }))
            .expect("tool tool_runs should be visible");
        let fast_observation_index = events
            .iter()
            .position(|event| {
                matches!(
                    event,
                    AgentEvent::Observation { tool_name, .. } if tool_name == "delay_fast"
                )
            })
            .expect("the fast result should be visible while the slow tool is running");
        let fast_tool_index = events
            .iter()
            .position(|event| {
                matches!(
                    event,
                    AgentEvent::ToolCall { tool_name, .. } if tool_name == "delay_fast"
                )
            })
            .expect("the fast tool call should be visible");
        assert!(
            thought_index < first_tool_index,
            "the complete text should be published before any tool card"
        );
        assert!(
            fast_tool_index < fast_observation_index,
            "the individual tool card should appear before its result"
        );
    }

    let history = tokio::time::timeout(std::time::Duration::from_secs(5), run)
        .await
        .expect("the slow tool run should finish")
        .expect("the session task should join")
        .unwrap();
    assert!(!history.is_empty());
    assert!(collector.has_observation("delay_slow"));
}

#[tokio::test]
async fn run_session_contains_custom_extension_panic() {
    let names = ["custom_panic"];
    let tools = Arc::new(ToolsFacade::new());
    for name in names {
        tools
            .share_services()
            .tool_catalog_test_support
            .register_installed_tool(Arc::new(PanicTool {
                tool_name: name.into(),
            }) as ToolHandle)
            .await
            .unwrap();
    }

    let calls = names
        .iter()
        .enumerate()
        .map(|(index, name)| CanonicalToolCall {
            tool_call_id: format!("panic-{index}"),
            name: (*name).into(),
            arguments: serde_json::json!({}),
        })
        .collect();
    let mock = Arc::new(ScriptedMock::new(vec![
        ScriptedResponse::Chunk(StreamChunk {
            tool_call_updates: Vec::new(),
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
            tool_call_updates: Vec::new(),
            text: Some("Recovered after extension failures.".into()),
            tool_calls: vec![CanonicalToolCall {
                tool_call_id: "final".into(),
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
        agent.run_session_from_id(&session.session_id),
    )
    .await
    .expect("extension panic session must not hang")
    .unwrap();
    assert!(!history.is_empty(), "the session must recover and continue");
    assert!(collector.has_tool_call("custom_panic"));
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
    let tools = Arc::new(ToolsFacade::new());
    tools
        .share_services()
        .authorization
        .set_execution_mode(haven_common::types::ExecutionMode::Collaborative)
        .await;
    tools
        .share_services()
        .authorization
        .set_boundaries(
            haven_common::types::SandboxMode::FullAccess,
            Vec::new(),
            haven_common::types::NetworkPolicy::Open,
        )
        .await;
    let mcp_manager = haven_tools::McpManager::new();
    let mcp_adapter = haven_tools::McpToolAdapter::new_panicking_for_test(
        mcp_manager,
        "panic-server",
        haven_tools::McpToolInfo {
            name: "panic_tool".into(),
            description: "panics for adapter-boundary testing".into(),
            input_schema: serde_json::json!({"type": "object"}),
        },
    );
    tools
        .share_services()
        .tool_catalog_test_support
        .register_installed_tool(Arc::new(mcp_adapter) as ToolHandle)
        .await
        .unwrap();

    let skill_root = tempfile::tempdir().unwrap();
    let skill_dir = skill_root.path().join("panic-skill");
    std::fs::create_dir_all(&skill_dir).unwrap();
    std::fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: \"panic-skill\"\ndescription: \"panics for adapter-boundary testing\"\nmetadata:\n  haven.language: \"python\"\n---\n\nTest fixture.\n",
    )
    .unwrap();
    let tool_services = tools.share_services();
    let skill_registry = Arc::clone(&tool_services.skills);
    skill_registry
        .set_config(
            Some(skill_root.path().to_path_buf()),
            Some(vec!["panic-skill".into()]),
        )
        .await
        .unwrap();
    let skill = Arc::new(
        tool_services
            .tool_catalog_test_support
            .get_skill_fixture("panic-skill")
            .await
            .expect("discover valid skill test fixture"),
    );
    let skill_config = haven_common::config::SkillsExecConfig::default();
    let skill_runner = haven_tools::SkillRunner::new(
        haven_tools::VenvManager::new(skill_config.venv_root.clone()),
        skill_config,
    );
    let skill_adapter = haven_tools::SkillToolAdapter::new_panicking_for_test(skill, skill_runner);
    tools
        .share_services()
        .tool_catalog_test_support
        .register_installed_tool(Arc::new(skill_adapter) as ToolHandle)
        .await
        .unwrap();

    let mcp_name = "mcp__panic-server__panic_5ftool";
    let skill_name = "skill__panic-skill";
    let mock = Arc::new(ScriptedMock::new(vec![
        ScriptedResponse::Chunk(StreamChunk {
            tool_call_updates: Vec::new(),
            text: Some("Running adapter checks.".into()),
            tool_calls: vec![
                CanonicalToolCall {
                    tool_call_id: "mcp-panic".into(),
                    name: mcp_name.into(),
                    arguments: serde_json::json!({}),
                },
                CanonicalToolCall {
                    tool_call_id: "skill-panic".into(),
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
            tool_call_updates: Vec::new(),
            text: Some("Recovered after adapter failures.".into()),
            tool_calls: vec![CanonicalToolCall {
                tool_call_id: "final".into(),
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

    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        agent.run_session_from_id(&session.session_id),
    )
    .await
    .expect("real adapter panic session must not hang")
    .unwrap();
    let confirmations = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            let requests = executor
                .pending_interactions(
                    &session.session_id,
                    crate::interaction::InteractionKind::Confirm,
                )
                .await;
            if requests.len() == 2 {
                break requests;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("untyped MCP and Skill operations should require one-time approvals");
    for request in &confirmations {
        executor
            .resolve_interaction(
                &session.session_id,
                &request.request_id,
                serde_json::Value::Bool(true),
                false,
            )
            .await
            .unwrap()
            .expect("confirmation should resolve");
    }
    let history = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        agent.run_session_from_id(&session.session_id),
    )
    .await
    .expect("approved adapter panic session must not hang")
    .unwrap();
    assert!(!history.is_empty(), "the session must recover and continue");
    assert!(collector.has_tool_call(mcp_name));
    assert!(collector.has_tool_call(skill_name));
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
    // replayed canonical transcript stays a valid assistant/tool chain.
    let tools = Arc::new(ToolsFacade::new());
    let timing = Arc::new(TimingState::new());
    tools
        .share_services()
        .tool_catalog_test_support
        .register_installed_tool(Arc::new(TimingTool::new("delay_a", timing.clone())) as ToolHandle)
        .await
        .unwrap();
    tools
        .share_services()
        .tool_catalog_test_support
        .register_installed_tool(Arc::new(TimingTool::new("delay_b", timing.clone())) as ToolHandle)
        .await
        .unwrap();
    let mock = Arc::new(ScriptedMock::new(vec![ScriptedResponse::Chunk(
        StreamChunk {
            tool_call_updates: Vec::new(),
            text: Some("Running both in parallel.".into()),
            tool_calls: vec![
                CanonicalToolCall {
                    tool_call_id: "tc1".into(),
                    name: "delay_a".into(),
                    arguments: serde_json::json!({}),
                },
                CanonicalToolCall {
                    tool_call_id: "tc2".into(),
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
        let session_id = session.session_id.clone();
        async move { agent.run_session_from_id(&session_id).await }
    });
    // Wait until both ToolCall events and both tool executions are visible,
    // then cancel while both tools (200ms sleeps) are still in flight.
    for _ in 0..50 {
        if collector.has_tool_call("delay_a")
            && collector.has_tool_call("delay_b")
            && timing.started.load(std::sync::atomic::Ordering::Acquire) == 2
        {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert!(
        collector.has_tool_call("delay_a") && collector.has_tool_call("delay_b"),
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
    executor.end_session(&session.session_id).await.unwrap();
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
    // The replayed canonical transcript stays a valid assistant/tool chain: no
    // dangling assistant tool_calls without a following result (which
    // providers would reject as a 400 on resume).
    let projection = load_event_projection(&agent, &session.session_id).await;
    let mut pending: Vec<String> = Vec::new();
    let mut interrupted_with_fields = 0;
    let canonical = projection.project().canonical_messages;
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
                    .map(|tc| tc.iter().map(|t| t.tool_call_id.clone()).collect())
                    .unwrap_or_default();
            }
            _ => {}
        }
    }
    assert_eq!(
        interrupted_with_fields, 2,
        "both interrupted tool results in the event projection must carry fields"
    );
    assert!(
        pending.is_empty(),
        "canonical transcript must not end with unanswered tool_calls (got {:?})",
        pending
    );
    // Pending step rows created at ToolCall emit must be completed with the
    // Interrupted observation so resume rebuilds the tool cards
    // from session_steps (not live-only UI state).
    let db_steps = agent.db.list_session_steps(&session.session_id).unwrap();
    let interrupted_db = db_steps
        .iter()
        .filter(|s| {
            s.tool_name.is_some()
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
            .map(|s| (s.tool_name.clone(), s.status, s.observation.clone()))
            .collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn turn_deadline_repairs_side_effecting_tool_before_resume() {
    let tools = Arc::new(ToolsFacade::new());
    let executions = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let started = Arc::new(tokio::sync::Notify::new());
    tools
        .share_services()
        .tool_catalog_test_support
        .register_installed_tool(Arc::new(DeadlineTool {
            tool_name: "deadline_effect",
            executions: executions.clone(),
            started: started.clone(),
            hold_open: true,
        }) as ToolHandle)
        .await
        .unwrap();
    let mock = Arc::new(ScriptedMock::new(vec![
        tool_batch_response(vec![CanonicalToolCall {
            tool_call_id: "effect-call".into(),
            name: "deadline_effect".into(),
            arguments: serde_json::json!({"operation": "non_idempotent"}),
        }]),
        final_answer_response(),
    ]));
    let limits = ContextLimitsConfig {
        turn_deadline_secs: 5,
        ..ContextLimitsConfig::default()
    };
    let (agent, executor) = make_test_agent_with_limits(mock.clone(), tools, limits);
    agent.set_emitter(Arc::new(EventCollector::new()));
    let session = executor
        .create_session("side effect before deadline")
        .await
        .unwrap();

    let mut first_run = tokio::spawn({
        let agent = agent.clone();
        let session_id = session.session_id.clone();
        async move { agent.run_session_from_id(&session_id).await }
    });
    tokio::select! {
        result = &mut first_run => panic!("agent run ended before the side effect started: {result:?}"),
        _ = started.notified() => {},
        _ = tokio::time::sleep(std::time::Duration::from_secs(4)) => {
            panic!("the non-idempotent side effect must happen before the deadline");
        }
    }
    let deadline_error = tokio::time::timeout(std::time::Duration::from_secs(7), first_run)
        .await
        .expect("the deadline must bound the first run")
        .expect("the run task must join")
        .expect_err("the blocked result must exceed the turn deadline");
    assert!(deadline_error.to_string().contains("deadline"));
    assert_eq!(
        executions.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "the side effect must have occurred exactly once before timeout"
    );

    let projection = load_event_projection(&agent, &session.session_id).await;
    let projected = projection.project().canonical_messages;
    assert_tool_calls_have_ordered_results(&projected, &["effect-call"]);
    assert!(projected.iter().any(|message| {
        message.role == CanonicalRole::Tool
            && message.tool_call_id.as_deref() == Some("effect-call")
            && message
                .content
                .iter()
                .any(|part| matches!(part, ContentPart::Text(text) if text.contains("Interrupted")))
    }));
    let step = agent
        .db
        .list_session_steps(&session.session_id)
        .unwrap()
        .into_iter()
        .find(|step| step.tool_call_id.as_deref() == Some("effect-call"))
        .expect("the attempted tool step must remain visible");
    assert_eq!(
        step.status,
        haven_common::lifecycle::SessionStepStatus::Unknown,
        "a side effect with no returned result must be recorded as unknown"
    );

    agent.continue_session(&session.session_id).await.unwrap();
    let resumed = tokio::time::timeout(
        std::time::Duration::from_secs(3),
        agent.run_session_from_id(&session.session_id),
    )
    .await
    .expect("the resumed turn should finish")
    .unwrap();
    assert!(!resumed.is_empty());
    let seen = mock.seen.lock().unwrap();
    assert_eq!(seen.len(), 2, "resume should make one model request");
    assert_tool_calls_have_ordered_results(&seen[1], &["effect-call"]);
    assert_eq!(
        executions.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "resume must not replay the result-unknown non-idempotent tool"
    );
}

#[tokio::test]
async fn turn_deadline_preserves_committed_parallel_result_and_repairs_running_sibling() {
    let tools = Arc::new(ToolsFacade::new());
    let fast_executions = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let slow_executions = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let fast_started = Arc::new(tokio::sync::Notify::new());
    let slow_started = Arc::new(tokio::sync::Notify::new());
    for (tool_name, executions, started, hold_open) in [
        (
            "deadline_fast",
            fast_executions.clone(),
            fast_started.clone(),
            false,
        ),
        (
            "deadline_slow",
            slow_executions.clone(),
            slow_started.clone(),
            true,
        ),
    ] {
        tools
            .share_services()
            .tool_catalog_test_support
            .register_installed_tool(Arc::new(DeadlineTool {
                tool_name,
                executions,
                started,
                hold_open,
            }) as ToolHandle)
            .await
            .unwrap();
    }
    let mock = Arc::new(ScriptedMock::new(vec![
        tool_batch_response(vec![
            CanonicalToolCall {
                tool_call_id: "fast-call".into(),
                name: "deadline_fast".into(),
                arguments: serde_json::json!({}),
            },
            CanonicalToolCall {
                tool_call_id: "slow-call".into(),
                name: "deadline_slow".into(),
                arguments: serde_json::json!({"operation": "non_idempotent"}),
            },
        ]),
        final_answer_response(),
    ]));
    let limits = ContextLimitsConfig {
        turn_deadline_secs: 5,
        ..ContextLimitsConfig::default()
    };
    let (agent, executor) = make_test_agent_with_limits(mock.clone(), tools, limits);
    agent.set_emitter(Arc::new(EventCollector::new()));
    let session = executor
        .create_session("partial parallel result at deadline")
        .await
        .unwrap();

    let mut first_run = tokio::spawn({
        let agent = agent.clone();
        let session_id = session.session_id.clone();
        async move { agent.run_session_from_id(&session_id).await }
    });
    tokio::select! {
        result = &mut first_run => panic!("agent run ended before both tools started: {result:?}"),
        _ = async { tokio::join!(fast_started.notified(), slow_started.notified()); } => {},
        _ = tokio::time::sleep(std::time::Duration::from_secs(4)) => {
            panic!("both parallel tool executions must start before the deadline");
        }
    }
    tokio::time::timeout(std::time::Duration::from_millis(300), async {
        loop {
            let projection = load_event_projection(&agent, &session.session_id).await;
            if projection.events.iter().any(|event| {
                matches!(event, TranscriptRecord::ToolResult { tool_call_id: Some(id), .. } if id == "fast-call")
            }) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the fast result must commit while its sibling remains blocked");

    let deadline_error = tokio::time::timeout(std::time::Duration::from_secs(7), first_run)
        .await
        .expect("the deadline must bound the parallel batch")
        .expect("the run task must join")
        .expect_err("the slow sibling must exceed the turn deadline");
    assert!(deadline_error.to_string().contains("deadline"));
    assert_eq!(fast_executions.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert_eq!(slow_executions.load(std::sync::atomic::Ordering::SeqCst), 1);

    let projection = load_event_projection(&agent, &session.session_id).await;
    let projected = projection.project().canonical_messages;
    assert_tool_calls_have_ordered_results(&projected, &["fast-call", "slow-call"]);
    assert!(projected.iter().any(|message| {
        message.role == CanonicalRole::Tool
            && message.tool_call_id.as_deref() == Some("slow-call")
            && message
                .content
                .iter()
                .any(|part| matches!(part, ContentPart::Text(text) if text.contains("Interrupted")))
    }));
    let steps = agent.db.list_session_steps(&session.session_id).unwrap();
    let fast_step = steps
        .iter()
        .find(|step| step.tool_call_id.as_deref() == Some("fast-call"))
        .expect("the committed sibling step must be present");
    let slow_step = steps
        .iter()
        .find(|step| step.tool_call_id.as_deref() == Some("slow-call"))
        .expect("the interrupted sibling step must be present");
    assert_eq!(
        fast_step.status,
        haven_common::lifecycle::SessionStepStatus::Completed
    );
    assert_eq!(
        slow_step.status,
        haven_common::lifecycle::SessionStepStatus::Unknown
    );

    agent.continue_session(&session.session_id).await.unwrap();
    let resumed = tokio::time::timeout(
        std::time::Duration::from_secs(3),
        agent.run_session_from_id(&session.session_id),
    )
    .await
    .expect("the resumed parallel turn should finish")
    .unwrap();
    assert!(!resumed.is_empty());
    let seen = mock.seen.lock().unwrap();
    assert_eq!(seen.len(), 2, "resume should make one model request");
    assert_tool_calls_have_ordered_results(&seen[1], &["fast-call", "slow-call"]);
    assert_eq!(
        fast_executions.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "the committed sibling must not execute again on resume"
    );
    assert_eq!(
        slow_executions.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "the result-unknown sibling must not execute again on resume"
    );
}
