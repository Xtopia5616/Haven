use super::support::*;
use super::*;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Mutex, Weak};

struct GenerationClient {
    label: &'static str,
    chats: AtomicUsize,
    embeddings: AtomicUsize,
    block_next: AtomicBool,
    entered: tokio::sync::Notify,
    release: tokio::sync::Notify,
}

#[async_trait]
impl LlmClient for GenerationClient {
    async fn chat(&self, _: Vec<CanonicalMessage>) -> Result<LlmResponse, LlmError> {
        self.chats.fetch_add(1, Ordering::SeqCst);
        if self.label == "revoked" {
            return Err(LlmError::Auth("test credential revoked".into()));
        }
        if self.block_next.swap(false, Ordering::SeqCst) {
            self.entered.notify_one();
            self.release.notified().await;
        }
        Ok(LlmResponse {
            text: self.label.into(),
            ..Default::default()
        })
    }

    async fn chat_with_output_cap(
        &self,
        messages: Vec<CanonicalMessage>,
        _: Option<u32>,
    ) -> Result<LlmResponse, LlmError> {
        self.chat(messages).await
    }

    async fn chat_stream(
        &self,
        _: Vec<CanonicalMessage>,
    ) -> Result<
        Pin<Box<dyn futures_util::Stream<Item = Result<StreamChunk, LlmError>> + Send>>,
        LlmError,
    > {
        Ok(Box::pin(stream::empty()))
    }

    async fn embed(&self, input: Vec<String>) -> Result<haven_llm::Embedding, LlmError> {
        self.embeddings.fetch_add(1, Ordering::SeqCst);
        if self.label == "revoked" {
            return Err(LlmError::Auth("test credential revoked".into()));
        }
        Ok(haven_llm::Embedding {
            vectors: input.iter().map(|_| vec![1.0, 0.0]).collect(),
            model: None,
            usage: Usage::default(),
        })
    }

    async fn health_check(&self) -> Result<(), LlmError> {
        Ok(())
    }
}

async fn generation(label: &'static str) -> (Arc<LlmRouter>, Arc<GenerationClient>) {
    let client = Arc::new(GenerationClient {
        label,
        chats: AtomicUsize::new(0),
        embeddings: AtomicUsize::new(0),
        block_next: AtomicBool::new(false),
        entered: tokio::sync::Notify::new(),
        release: tokio::sync::Notify::new(),
    });
    let router = Arc::new(LlmRouter::new_with_test_clients_and_embedding(
        client.clone(),
        client.clone(),
        client.clone(),
        client.clone(),
        client.clone(),
    ));
    router
        .set_request_configured_for_test(haven_common::config::RequestKind::FastChat, true)
        .await;
    router
        .set_request_configured_for_test(haven_common::config::RequestKind::Embedding, true)
        .await;
    (router, client)
}

#[tokio::test]
async fn router_reload_reaches_title_inference_prompt_recall_and_index() {
    let db = Arc::new(Database::open_in_memory().unwrap());
    let tools = Arc::new(ToolsFacade::new());
    let executor = Arc::new(SessionSupervisor::new_for_test(
        db.clone(),
        tools.clone(),
        1,
    ));
    let (a, client_a) = generation("A").await;
    let limits = ContextLimitsConfig::default();
    let memory = memory_service_for_test(db.clone(), a.clone(), &limits);
    let agent = AgentLayer::build(
        memory.clone(),
        executor,
        crate::AgentToolPorts::from_tools_facade(tools),
        a.clone(),
        10,
        20,
        limits,
    )
    .agent;
    let title = agent.title.as_ref().unwrap().clone();
    assert_eq!(
        title.generate(&["hello".into()]).await.as_deref(),
        Some("A")
    );
    memory
        .prompt_candidates("Rust preference", None)
        .await
        .unwrap();
    assert!(
        memory
            .has_cached_prompt_candidates("Rust preference", None)
            .await
    );
    let snapshot_a = agent.react_engine.router();
    client_a.block_next.store(true, Ordering::SeqCst);
    let old_title = title.clone();
    let in_flight = tokio::spawn(async move { old_title.generate(&["in flight".into()]).await });
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        client_a.entered.notified(),
    )
    .await
    .unwrap();

    let (b, client_b) = generation("B").await;
    agent.replace_router_for_test(b.clone()).unwrap();
    client_a.release.notify_one();
    assert_eq!(in_flight.await.unwrap().as_deref(), Some("A"));
    assert!(Arc::ptr_eq(&agent.react_engine.router(), &b));
    assert!(Arc::ptr_eq(&snapshot_a, &a));
    assert_eq!(
        title.generate(&["hello".into()]).await.as_deref(),
        Some("B")
    );
    assert_eq!(agent.memory_worker.fast_chat_for_test().await.unwrap(), "B");
    assert_eq!(
        agent
            .react_engine
            .router()
            .chat_with_prompt_request(haven_llm::PromptRequest::new(
                haven_common::config::RequestKind::Chat,
                "system",
                "new chat"
            ))
            .await
            .unwrap()
            .text,
        "B"
    );
    assert!(
        !memory
            .has_cached_prompt_candidates("Rust preference", None)
            .await
    );
    memory
        .prompt_candidates("Rust preference", None)
        .await
        .unwrap();
    memory
        .recall(
            haven_memory::MemoryQuery::new("Rust", haven_memory::MemoryRecallKind::Semantic, 5)
                .unwrap(),
        )
        .await
        .unwrap();
    db.insert_fact("user", "likes", "Rust", "inferred", 1.0, &[])
        .unwrap();
    memory.embed_new_memory().await;
    assert_eq!(client_a.chats.load(Ordering::SeqCst), 2);
    assert_eq!(client_a.embeddings.load(Ordering::SeqCst), 1);
    assert_eq!(client_b.chats.load(Ordering::SeqCst), 3);
    assert_eq!(client_b.embeddings.load(Ordering::SeqCst), 3);

    let (revoked, revoked_client) = generation("revoked").await;
    agent.replace_router_for_test(revoked).unwrap();
    assert!(title.generate(&["hello".into()]).await.is_none());
    assert!(agent.memory_worker.fast_chat_for_test().await.is_err());
    memory
        .recall(
            haven_memory::MemoryQuery::new("Rust", haven_memory::MemoryRecallKind::Semantic, 5)
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(revoked_client.embeddings.load(Ordering::SeqCst), 1);
    assert_eq!(client_b.chats.load(Ordering::SeqCst), 3);
    assert_eq!(client_b.embeddings.load(Ordering::SeqCst), 3);

    let unconfigured = Arc::new(LlmRouter::new(haven_common::config::RouterConfig::default()));
    agent.replace_router_for_test(unconfigured).unwrap();
    assert!(title.generate(&["hello".into()]).await.is_none());
    assert_eq!(memory.current_embedding_model().await, "");
    memory
        .recall(
            haven_memory::MemoryQuery::new("Rust", haven_memory::MemoryRecallKind::Semantic, 5)
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(agent.memory_worker.fast_chat_for_test().await.is_err());
    assert_eq!(client_b.embeddings.load(Ordering::SeqCst), 3);
    agent.shutdown_background_tasks().await;
}

struct SwapRouterDuringTool {
    agent: Arc<Mutex<Option<Weak<AgentLayer>>>>,
    next_router: Arc<LlmRouter>,
}

#[async_trait]
impl Tool for SwapRouterDuringTool {
    fn name(&self) -> String {
        "swap_model_generation".into()
    }

    fn description(&self) -> String {
        "Publishes the next model generation during a ReAct run".into()
    }

    fn risk_level(&self, _: &serde_json::Value) -> RiskLevel {
        RiskLevel::Safe
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({"type": "object"})
    }

    async fn execute(
        &self,
        _: serde_json::Value,
        _: CancellationToken,
    ) -> anyhow::Result<ToolResult> {
        let agent = self
            .agent
            .lock()
            .unwrap()
            .as_ref()
            .and_then(Weak::upgrade)
            .expect("test agent is installed before the ReAct run");
        agent.replace_router_for_test(self.next_router.clone())?;
        Ok(ToolResult::ok(serde_json::json!({"published": true})))
    }
}

fn tool_call_response(name: &str, id: &str, text: &str) -> ScriptedResponse {
    ScriptedResponse::Chunk(StreamChunk {
        tool_call_updates: Vec::new(),
        text: Some(text.into()),
        tool_calls: vec![CanonicalToolCall {
            tool_call_id: id.into(),
            name: name.into(),
            arguments: serde_json::json!({}),
        }],
        finish_reason: Some(FinishReason::ToolCalls),
        usage: None,
        model: None,
        reasoning: None,
        web_search: None,
        web_search_calls: Vec::new(),
        thinking_blocks: Vec::new(),
    })
}

#[tokio::test]
async fn one_react_run_uses_the_latest_model_generation_for_each_request() {
    let client_a = Arc::new(ScriptedMock::new(vec![tool_call_response(
        "swap_model_generation",
        "switch-generation",
        "Publishing the next model generation.",
    )]));
    let client_b = Arc::new(ScriptedMock::new(vec![tool_call_response(
        "final_answer",
        "finish-with-latest-generation",
        "Answered with the latest model generation.",
    )]));
    let router_a = Arc::new(LlmRouter::new_with_test_clients(
        client_a.clone(),
        client_a.clone(),
        client_a.clone(),
        client_a.clone(),
    ));
    let router_b = Arc::new(LlmRouter::new_with_test_clients(
        client_b.clone(),
        client_b.clone(),
        client_b.clone(),
        client_b.clone(),
    ));
    let database = Arc::new(Database::open_in_memory().unwrap());
    let tools = Arc::new(ToolsFacade::new());
    let agent_slot = Arc::new(Mutex::new(None));
    tools
        .share_services()
        .tool_catalog_test_support
        .register_installed_tool(Arc::new(SwapRouterDuringTool {
            agent: agent_slot.clone(),
            next_router: router_b,
        }) as ToolHandle)
        .await
        .unwrap();
    let executor = Arc::new(SessionSupervisor::new_for_test(
        database.clone(),
        tools.clone(),
        1,
    ));
    let limits = ContextLimitsConfig::default();
    let memory = memory_service_for_test(database, router_a.clone(), &limits);
    let startup = AgentLayer::build(
        memory,
        executor.clone(),
        crate::AgentToolPorts::from_tools_facade(tools),
        router_a,
        10,
        20,
        limits,
    );
    let agent = Arc::new(startup.agent);
    *agent_slot.lock().unwrap() = Some(Arc::downgrade(&agent));
    agent.set_emitter(make_recording_emitter());
    let session = executor
        .create_session("model configuration changes mid-run")
        .await
        .unwrap();

    agent
        .run_session_from_id(&session.session_id)
        .await
        .unwrap();

    assert_eq!(client_a.seen.lock().unwrap().len(), 1);
    assert_eq!(
        client_b.seen.lock().unwrap().len(),
        1,
        "the next request in the same active ReAct run must capture the newly published generation"
    );
    assert_eq!(
        executor
            .get_active_session_status(&session.session_id)
            .await,
        Some(SessionStatus::Paused)
    );
    agent.shutdown_background_tasks().await;
}
