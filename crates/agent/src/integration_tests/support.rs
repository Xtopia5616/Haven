use super::*;

pub(super) fn temp_db() -> Arc<Database> {
    let mut p = std::env::temp_dir();
    p.push(format!("haven_agent_test_{}.db", uuid::Uuid::new_v4()));
    Arc::new(Database::open(&p).unwrap())
}

/// Mock LlmClient whose `chat_stream_with_tools` returns a single chunk
/// containing the `final_answer` tool call so the ReAct loop terminates
/// in one step.
pub(super) struct FinalAnswerMock;

#[async_trait]
impl LlmClient for FinalAnswerMock {
    async fn chat(&self, _: Vec<CanonicalMessage>) -> Result<LlmResponse, LlmError> {
        Err(LlmError::Unknown("mock: chat not implemented".into()))
    }
    async fn chat_with_tools(
        &self,
        _: Vec<CanonicalMessage>,
        _: Vec<ToolDefinition>,
    ) -> Result<LlmResponse, LlmError> {
        Err(LlmError::Unknown(
            "mock: chat_with_tools not implemented".into(),
        ))
    }
    async fn chat_stream(
        &self,
        _: Vec<CanonicalMessage>,
    ) -> Result<
        Pin<Box<dyn futures_util::Stream<Item = Result<StreamChunk, LlmError>> + Send>>,
        LlmError,
    > {
        Err(LlmError::Unknown(
            "mock: chat_stream not implemented".into(),
        ))
    }
    async fn chat_stream_with_tools(
        &self,
        _: Vec<CanonicalMessage>,
        _: Vec<ToolDefinition>,
    ) -> Result<
        Pin<Box<dyn futures_util::Stream<Item = Result<StreamChunk, LlmError>> + Send>>,
        LlmError,
    > {
        let chunk = StreamChunk {
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
        };
        Ok(Box::pin(stream::iter(vec![Ok(chunk)])))
    }
    async fn health_check(&self) -> Result<(), LlmError> {
        Ok(())
    }
}

pub(super) struct RecordingEmitter {
    pub(super) thoughts: std::sync::Mutex<Vec<String>>,
    pub(super) supplements: std::sync::Mutex<Vec<String>>,
    pub(super) notifications: std::sync::Mutex<Vec<(String, String)>>,
    pub(super) completed: std::sync::Mutex<bool>,
}

#[async_trait]
impl AgentEventEmitter for RecordingEmitter {
    async fn emit(&self, event: AgentEvent) {
        match event {
            AgentEvent::Thought { thought, .. } => {
                self.thoughts.lock().unwrap().push(thought);
            }
            AgentEvent::SessionCompleted { .. } => {
                *self.completed.lock().unwrap() = true;
            }
            AgentEvent::SessionUpdated { .. } => {
                *self.completed.lock().unwrap() = true;
            }
            AgentEvent::Supplement {
                additional_context, ..
            } => {
                self.supplements.lock().unwrap().push(additional_context);
            }
            AgentEvent::Notification { title, body, .. } => {
                self.notifications.lock().unwrap().push((title, body));
            }
            _ => {}
        }
    }
}

pub(super) fn make_test_agent() -> (Arc<AgentLayer>, Arc<SessionExecutor>) {
    let mut p = std::env::temp_dir();
    p.push(format!("haven_agent_test_{}.db", uuid::Uuid::new_v4()));
    let db = Arc::new(Database::open(&p).unwrap());
    let tools = Arc::new(ToolsManager::new());
    let executor = Arc::new(SessionExecutor::new(db.clone(), tools, 1));
    let client = Arc::new(FinalAnswerMock) as Arc<dyn LlmClient>;
    let router = Arc::new(LlmRouter::new_with_clients(
        client.clone(),
        client.clone(),
        client.clone(),
        client,
    ));
    let agent = Arc::new(AgentLayer::new(
        db,
        executor.clone(),
        router,
        30,
        50,
        ContextLimitsConfig::default(),
    ));
    (agent, executor)
}

pub(super) fn make_recording_emitter() -> Arc<RecordingEmitter> {
    Arc::new(RecordingEmitter {
        thoughts: std::sync::Mutex::new(Vec::new()),
        supplements: std::sync::Mutex::new(Vec::new()),
        notifications: std::sync::Mutex::new(Vec::new()),
        completed: std::sync::Mutex::new(false),
    })
}

pub(super) fn make_canonical(role: CanonicalRole, text: &str) -> CanonicalMessage {
    CanonicalMessage {
        role,
        content: vec![ContentPart::text(text.to_string())],
        tool_calls: None,
        tool_call_id: None,
        reasoning: None,
        web_search_calls: Vec::new(),
        thinking_blocks: Vec::new(),
        source: None,
        id: None,
    }
}

pub(super) fn make_assistant_with_calls(ids: &[&str]) -> CanonicalMessage {
    let mut m = make_canonical(CanonicalRole::Assistant, "");
    m.tool_calls = Some(
        ids.iter()
            .map(|id| CanonicalToolCall {
                id: id.to_string(),
                name: "tool".into(),
                arguments: serde_json::Value::Null,
            })
            .collect(),
    );
    m
}

pub(super) fn make_tool_result(call_id: &str, text: &str) -> CanonicalMessage {
    let mut m = make_canonical(CanonicalRole::Tool, text);
    m.tool_call_id = Some(call_id.to_string());
    m
}

pub(super) fn make_test_agent_with(
    client: Arc<dyn LlmClient>,
    tools: Arc<ToolsManager>,
) -> (Arc<AgentLayer>, Arc<SessionExecutor>) {
    let mut p = std::env::temp_dir();
    p.push(format!("haven_agent_test_{}.db", uuid::Uuid::new_v4()));
    let db = Arc::new(Database::open(&p).unwrap());
    let executor = Arc::new(SessionExecutor::new(db.clone(), tools, 1));
    let router = Arc::new(LlmRouter::new_with_clients(
        client.clone(),
        client.clone(),
        client.clone(),
        client,
    ));
    let agent = Arc::new(AgentLayer::new(
        db,
        executor.clone(),
        router,
        30,
        50,
        ContextLimitsConfig::default(),
    ));
    (agent, executor)
}

/// A mock tool whose schema requires an `action` field, mirroring the
/// production failure where a call's arguments are missing a required
/// discriminator field and the provider rejects the request body.
pub(super) struct ActionRequiredTool;

#[async_trait]
impl Tool for ActionRequiredTool {
    fn name(&self) -> String {
        "action_required".into()
    }
    fn description(&self) -> String {
        "requires an action".into()
    }
    fn risk_level(&self, _: &serde_json::Value) -> RiskLevel {
        RiskLevel::Safe
    }
    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["go", "stop"],
                    "default": "go"
                },
                "query": { "type": "string" }
            },
            "required": ["action", "query"]
        })
    }
    async fn execute(
        &self,
        _: serde_json::Value,
        _: CancellationToken,
    ) -> anyhow::Result<ToolResult> {
        Ok(ToolResult::ok(serde_json::json!({"ok": true})))
    }
}

/// A mock tool whose required field is enum-constrained with NO schema
/// default, mirroring the `input` tool's `operation` discriminator.
/// An absent enum discriminator is invalid; validation must not select the
/// first enum value because that could change the requested side effect.
pub(super) struct EnumRequiredTool;

#[async_trait]
impl Tool for EnumRequiredTool {
    fn name(&self) -> String {
        "enum_required".into()
    }
    fn description(&self) -> String {
        "requires an enum value".into()
    }
    fn risk_level(&self, _: &serde_json::Value) -> RiskLevel {
        RiskLevel::Safe
    }
    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "operation": {
                    "type": "string",
                    "enum": ["type", "key", "click"]
                }
            },
            "required": ["operation"]
        })
    }
    async fn execute(
        &self,
        _: serde_json::Value,
        _: CancellationToken,
    ) -> anyhow::Result<ToolResult> {
        Ok(ToolResult::ok(serde_json::json!({"ok": true})))
    }
}

/// A mock tool with an optional enum-constrained field: the value is
/// validated when present, but the field itself is not required.
pub(super) struct EnumWithOptionalTool;

#[async_trait]
impl Tool for EnumWithOptionalTool {
    fn name(&self) -> String {
        "enum_with_optional".into()
    }
    fn description(&self) -> String {
        "optional enum field".into()
    }
    fn risk_level(&self, _: &serde_json::Value) -> RiskLevel {
        RiskLevel::Safe
    }
    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "operation": {
                    "type": "string",
                    "enum": ["type", "key", "click"]
                },
                "optional": {
                    "type": "string",
                    "enum": ["a", "b", "c"]
                }
            },
            "required": ["operation"]
        })
    }
    async fn execute(
        &self,
        _: serde_json::Value,
        _: CancellationToken,
    ) -> anyhow::Result<ToolResult> {
        Ok(ToolResult::ok(serde_json::json!({"ok": true})))
    }
}

/// Scripted LlmClient that returns a pre-programmed sequence of responses
/// from `chat_stream_with_tools`, enabling full ReAct-loop integration
/// tests without a live LLM. Mirrors Pi's `MockLlmClient` pattern.
pub(super) struct ScriptedMock {
    pub(super) stream_responses: std::sync::Mutex<VecDeque<ScriptedResponse>>,
    pub(super) chat_text: std::sync::Mutex<String>,
    /// Every message batch sent to `chat_stream_with_tools`, for
    /// assertions (e.g. that no dangling tool_call is sent).
    pub(super) seen: std::sync::Mutex<Vec<Vec<CanonicalMessage>>>,
}

pub(super) enum ScriptedResponse {
    Err(LlmError),
    Chunk(StreamChunk),
    ChunkThenErr(StreamChunk, LlmError),
    /// Yield the chunk only after `delay_ms`, so a test can deliver
    /// steering/supplements while the LLM call is in flight.
    ChunkDelayed(StreamChunk, u64),
}

impl ScriptedMock {
    pub(super) fn new(responses: Vec<ScriptedResponse>) -> Self {
        Self {
            stream_responses: std::sync::Mutex::new(VecDeque::from(responses)),
            chat_text: std::sync::Mutex::new("Compacted summary.".into()),
            seen: std::sync::Mutex::new(Vec::new()),
        }
    }
}

#[async_trait]
impl LlmClient for ScriptedMock {
    async fn chat(&self, _: Vec<CanonicalMessage>) -> Result<LlmResponse, LlmError> {
        let text = self.chat_text.lock().unwrap().clone();
        Ok(LlmResponse {
            text,
            tool_calls: vec![],
            finish_reason: Some(FinishReason::Stop),
            usage: Usage::default(),
            model: None,
            reasoning: None,
            web_search_calls: Vec::new(),
            thinking_blocks: Vec::new(),
        })
    }
    async fn chat_with_tools(
        &self,
        _: Vec<CanonicalMessage>,
        _: Vec<ToolDefinition>,
    ) -> Result<LlmResponse, LlmError> {
        Err(LlmError::Unknown("mock: use chat_stream_with_tools".into()))
    }
    async fn chat_stream(
        &self,
        _: Vec<CanonicalMessage>,
    ) -> Result<
        Pin<Box<dyn futures_util::Stream<Item = Result<StreamChunk, LlmError>> + Send>>,
        LlmError,
    > {
        Err(LlmError::Unknown("mock: use chat_stream_with_tools".into()))
    }
    async fn chat_stream_with_tools(
        &self,
        messages: Vec<CanonicalMessage>,
        _: Vec<ToolDefinition>,
    ) -> Result<
        Pin<Box<dyn futures_util::Stream<Item = Result<StreamChunk, LlmError>> + Send>>,
        LlmError,
    > {
        self.seen.lock().unwrap().push(messages);
        let resp =
            self.stream_responses
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or(ScriptedResponse::Err(LlmError::Unknown(
                    "scripted responses exhausted".into(),
                )));
        match resp {
            ScriptedResponse::Err(e) => Err(e),
            ScriptedResponse::Chunk(chunk) => Ok(Box::pin(stream::iter(vec![Ok(chunk)]))),
            ScriptedResponse::ChunkThenErr(chunk, e) => {
                Ok(Box::pin(stream::iter(vec![Ok(chunk), Err(e)])))
            }
            ScriptedResponse::ChunkDelayed(chunk, delay_ms) => {
                Ok(Box::pin(stream::once(async move {
                    tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
                    Ok(chunk)
                })))
            }
        }
    }
    async fn health_check(&self) -> Result<(), LlmError> {
        Ok(())
    }
}

pub(super) struct EventCollector {
    pub(super) events: std::sync::Mutex<Vec<AgentEvent>>,
}

impl EventCollector {
    pub(super) fn new() -> Self {
        Self {
            events: std::sync::Mutex::new(Vec::new()),
        }
    }
    pub(super) fn has_action(&self, tool_name: &str) -> bool {
        self.events
            .lock()
            .unwrap()
            .iter()
            .any(|e| matches!(e, AgentEvent::Action { tool_name: tn, .. } if tn == tool_name))
    }
    pub(super) fn has_observation(&self, tool_name: &str) -> bool {
        self.events
            .lock()
            .unwrap()
            .iter()
            .any(|e| matches!(e, AgentEvent::Observation { tool_name: tn, .. } if tn == tool_name))
    }
    pub(super) fn interrupted_observations(&self) -> Vec<String> {
        self.events
            .lock()
            .unwrap()
            .iter()
            .filter_map(|e| {
                if let AgentEvent::Observation { observation, .. } = e {
                    if observation.contains("Interrupted") {
                        Some(observation.clone())
                    } else {
                        None
                    }
                } else {
                    None
                }
            })
            .collect()
    }
    pub(super) fn has_compaction(&self) -> bool {
        self.events
            .lock()
            .unwrap()
            .iter()
            .any(|e| matches!(e, AgentEvent::Compaction { .. }))
    }
    pub(super) fn has_notification(&self) -> Option<(String, String)> {
        self.events.lock().unwrap().iter().find_map(|e| {
            if let AgentEvent::Notification { title, body, .. } = e {
                Some((title.clone(), body.clone()))
            } else {
                None
            }
        })
    }
}

#[async_trait]
impl AgentEventEmitter for EventCollector {
    async fn emit(&self, event: AgentEvent) {
        self.events.lock().unwrap().push(event);
    }
}

pub(super) struct EchoTool;

#[async_trait]
impl Tool for EchoTool {
    fn name(&self) -> String {
        "echo".into()
    }
    fn description(&self) -> String {
        "Echo back the input text".into()
    }
    fn risk_level(&self, _: &serde_json::Value) -> RiskLevel {
        RiskLevel::Safe
    }
    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({"type": "object", "properties": {"text": {"type": "string"}}})
    }
    async fn execute(
        &self,
        input: serde_json::Value,
        _: CancellationToken,
    ) -> anyhow::Result<ToolResult> {
        Ok(ToolResult::ok(
            serde_json::json!({"echoed": input["text"].as_str().unwrap_or("")}),
        ))
    }
}

pub(super) struct TimingState {
    pub(super) intervals: std::sync::Mutex<Vec<(Instant, Instant)>>,
    pub(super) started: std::sync::atomic::AtomicUsize,
}

impl TimingState {
    pub(super) fn new() -> Self {
        Self {
            intervals: std::sync::Mutex::new(Vec::new()),
            started: std::sync::atomic::AtomicUsize::new(0),
        }
    }
}

pub(super) struct TimingTool {
    tool_name: String,
    state: Arc<TimingState>,
}

impl TimingTool {
    pub(super) fn new(name: &str, state: Arc<TimingState>) -> Self {
        Self {
            tool_name: name.into(),
            state,
        }
    }
}

#[async_trait]
impl Tool for TimingTool {
    fn name(&self) -> String {
        self.tool_name.clone()
    }
    fn description(&self) -> String {
        "Delayed tool for parallel testing".into()
    }
    fn risk_level(&self, _: &serde_json::Value) -> RiskLevel {
        RiskLevel::Safe
    }
    fn concurrency(&self, _: &serde_json::Value) -> ToolConcurrency {
        ToolConcurrency::ReadOnly
    }
    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({"type": "object"})
    }
    async fn execute(
        &self,
        _: serde_json::Value,
        _: CancellationToken,
    ) -> anyhow::Result<ToolResult> {
        self.state
            .started
            .fetch_add(1, std::sync::atomic::Ordering::Release);
        let start = Instant::now();
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        self.state
            .intervals
            .lock()
            .unwrap()
            .push((start, Instant::now()));
        Ok(ToolResult::ok(serde_json::json!({"ok": true})))
    }
}

/// Seed the common rollback-test fixture: messages `hello` / `thinking` /
/// `interrupt` plus a saved ReAct snapshot at step 1 (canonical =
/// [System "sys", User "hello"], branch point after the thinking turn).
/// Returns the persisted messages so tests can resolve specific ids.
pub(super) fn seed_hello_snapshot(
    agent: &AgentLayer,
    session_id: &str,
) -> Vec<haven_memory::repositories::messages::Message> {
    agent
        .db
        .add_message(session_id, "user", "hello", Some("text"), None)
        .unwrap();
    agent
        .db
        .add_message(session_id, "assistant", "thinking", Some("text"), None)
        .unwrap();
    agent
        .db
        .add_message(session_id, "user", "interrupt", Some("text"), None)
        .unwrap();
    let msgs = agent.db.get_session_messages(session_id).unwrap();
    let thinking_ts = msgs
        .iter()
        .find(|m| m.role == "assistant")
        .unwrap()
        .created_at
        .clone();
    let canonical = vec![
        CanonicalMessage {
            role: CanonicalRole::System,
            content: vec![ContentPart::text("sys")],
            tool_calls: None,
            tool_call_id: None,
            reasoning: None,
            web_search_calls: Vec::new(),
            thinking_blocks: Vec::new(),
            source: None,
            id: None,
        },
        CanonicalMessage {
            role: CanonicalRole::User,
            content: vec![ContentPart::text("hello")],
            tool_calls: None,
            tool_call_id: None,
            reasoning: None,
            web_search_calls: Vec::new(),
            thinking_blocks: Vec::new(),
            source: None,
            id: None,
        },
    ];
    let mut branch_points = HashMap::new();
    branch_points.insert(
        1,
        BranchPoint {
            event_cursor: 1, // CompactSummary seed length
            step_number: 1,
            last_msg_at: Some(thinking_ts),
        },
    );
    let snapshot = ReActSnapshot {
        events: seed_events_from_canonical(canonical),
        step_number: 1,
        branch_points,
        saved_at: None,
        awaiting_answer: None,
        awaiting_confirm: None,
        run_budget: None,
        error_partial_message_ids: None,
    };
    agent
        .db
        .save_react_state(session_id, &serde_json::to_string(&snapshot).unwrap())
        .unwrap();
    msgs
}
