use super::*;
use async_trait::async_trait;
use haven_common::types::InjectSource;
use haven_llm::{LlmClient, LlmError, StreamChunk, Usage};
use haven_tools::ToolsFacade;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio_util::sync::CancellationToken;

type TestStream = Pin<Box<dyn futures_util::Stream<Item = Result<StreamChunk, LlmError>> + Send>>;

struct PreparationClient {
    seen: Mutex<Vec<Arc<[CanonicalMessage]>>>,
    summaries: AtomicUsize,
    executor: Arc<SessionSupervisor>,
    session_id: String,
}

#[async_trait]
impl LlmClient for PreparationClient {
    async fn chat(&self, _: Vec<CanonicalMessage>) -> Result<LlmResponse, LlmError> {
        self.summaries.fetch_add(1, Ordering::SeqCst);
        // New steering during the optional late compaction must wait. A
        // re-poll loop here could keep postponing the provider indefinitely.
        self.executor
            .add_steering(&self.session_id, "during-rebuild")
            .await
            .unwrap();
        Ok(LlmResponse {
            text: "summary".into(),
            usage: Usage::default(),
            ..Default::default()
        })
    }

    async fn chat_with_tools(
        &self,
        messages: Vec<CanonicalMessage>,
        _: Vec<LlmToolDefinition>,
    ) -> Result<LlmResponse, LlmError> {
        self.chat(messages).await
    }

    async fn chat_with_output_cap(
        &self,
        messages: Vec<CanonicalMessage>,
        _: Option<u32>,
    ) -> Result<LlmResponse, LlmError> {
        self.chat(messages).await
    }

    async fn chat_stream(&self, messages: Vec<CanonicalMessage>) -> Result<TestStream, LlmError> {
        self.chat_stream_with_tools(messages, Vec::new()).await
    }

    async fn chat_stream_with_tools(
        &self,
        messages: Vec<CanonicalMessage>,
        _: Vec<LlmToolDefinition>,
    ) -> Result<TestStream, LlmError> {
        self.chat_stream_with_tools_output_cap_shared(
            messages.into(),
            Arc::from(Vec::<LlmToolDefinition>::new()),
            None,
        )
        .await
    }

    async fn chat_stream_with_tools_output_cap_shared(
        &self,
        messages: Arc<[CanonicalMessage]>,
        _: Arc<[LlmToolDefinition]>,
        _: Option<u32>,
    ) -> Result<TestStream, LlmError> {
        self.seen.lock().unwrap().push(messages);
        Ok(Box::pin(futures_util::stream::iter(vec![Ok(
            StreamChunk {
                text: Some("done".into()),
                finish_reason: Some(FinishReason::Stop),
                ..Default::default()
            },
        )])))
    }

    async fn health_check(&self) -> Result<(), LlmError> {
        Ok(())
    }
}

struct PreparationProbe {
    items: usize,
    cancel: bool,
    calls: AtomicUsize,
}

#[async_trait]
impl super::turn_preparation::TurnPreparation for PreparationProbe {
    async fn prepare(
        &self,
        engine: &ReActEngine,
        ctx: &StepCtx,
        _: &mut ReActState,
        cancel: CancellationToken,
    ) -> anyhow::Result<super::turn_preparation::PreparedTurn> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        // Deterministically model input accepted during an awaited prologue.
        for index in 0..self.items {
            engine
                .executor
                .add_steering(&ctx.session_id, &format!("late-{index}"))
                .await?;
        }
        engine
            .executor
            .add_follow_up(&ctx.session_id, "after-work")
            .await?;
        if self.cancel {
            cancel.cancel();
        }
        let catalog = engine.build_tool_catalog_for_session(&ctx.session_id).await;
        Ok(super::turn_preparation::PreparedTurn::from_catalog(
            engine,
            &ctx.session_id,
            catalog,
            None,
        ))
    }
}

struct SilentEmitter;
#[async_trait]
impl AgentEventEmitter for SilentEmitter {
    async fn emit(&self, _: AgentEvent) {}
}

async fn run_preparation_case(
    items: usize,
    compact: bool,
    cancel: bool,
) -> (Arc<PreparationClient>, Arc<PreparationProbe>, ReActState) {
    let directory = tempfile::tempdir().unwrap();
    let db = Arc::new(Database::open(&directory.path().join("preparation.db")).unwrap());
    let executor = Arc::new(SessionSupervisor::new_for_test(
        db.clone(),
        Arc::new(ToolsFacade::new()),
        1,
    ));
    let session = executor.create_session("preparation").await.unwrap();
    executor
        .update_session_status(&session.session_id, SessionStatus::Running)
        .await
        .unwrap();
    let client = Arc::new(PreparationClient {
        seen: Mutex::new(Vec::new()),
        summaries: AtomicUsize::new(0),
        executor: executor.clone(),
        session_id: session.session_id.clone(),
    });
    let router = Arc::new(LlmRouter::new_with_test_clients(
        client.clone(),
        client.clone(),
        client.clone(),
        client.clone(),
    ));
    let hook = Arc::new(PreparationProbe {
        items,
        cancel,
        calls: AtomicUsize::new(0),
    });
    let engine = ReActEngine::new(
        router,
        test_tool_catalog_port(&executor),
        executor,
        MemoryOutboxStore::new(db),
        1,
        ContextLimitsConfig {
            compaction_ratio: 0.1,
            ..Default::default()
        },
    )
    .with_turn_preparation(hook.clone());
    let canonical = if compact {
        (0..12)
            .map(|index| {
                CanonicalMessage::user_text(format!(
                    "history-{index} {}",
                    "old context ".repeat(5000)
                ))
            })
            .collect()
    } else {
        vec![CanonicalMessage::user_text("work")]
    };
    let mut state = ReActState::new(Vec::new(), canonical, HashMap::new());
    tokio::time::timeout(
        std::time::Duration::from_secs(30),
        engine.run_react_loop(
            ReActRunInput {
                session_id: session.session_id,
                start_step: 1,
                emitter: Arc::new(SilentEmitter),
                run_id: 1,
                is_session_start: false,
                run_source: haven_common::hooks::LifecycleHookRunSource::NewInput,
            },
            &mut state,
        ),
    )
    .await
    .expect("bounded steering preparation must reach or cancel the provider")
    .unwrap();
    (client, hook, state)
}

#[tokio::test]
async fn steering_received_during_preparation_reaches_the_first_request() {
    let (client, hook, _) = run_preparation_case(1, false, false).await;
    let seen = client.seen.lock().unwrap();
    assert_eq!(seen.len(), 1);
    assert!(
        seen[0]
            .iter()
            .any(|message| message.source == Some(InjectSource::Steering))
    );
    assert!(
        !seen[0]
            .iter()
            .any(|message| message.source == Some(InjectSource::FollowUp))
    );
    assert_eq!(hook.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn late_steering_poll_is_limited_to_one_batch() {
    let (client, hook, _) =
        run_preparation_case(crate::session::CONTEXT_BATCH_MAX_ITEMS + 1, false, false).await;
    let seen = client.seen.lock().unwrap();
    assert_eq!(seen.len(), 1, "a full queue must not starve the provider");
    assert_eq!(
        seen[0]
            .iter()
            .filter(|message| message.source == Some(InjectSource::Steering))
            .count(),
        crate::session::CONTEXT_BATCH_MAX_ITEMS
    );
    assert_eq!(
        hook.calls.load(Ordering::SeqCst),
        1,
        "do not repeat prologue side effects"
    );
}

#[tokio::test]
async fn late_steering_rechecks_compaction_and_does_not_repoll_afterwards() {
    let (client, _, state) = run_preparation_case(1, true, false).await;
    assert_eq!(
        state
            .events
            .iter()
            .filter(|event| matches!(event, TranscriptRecord::CompactSummary { .. }))
            .count(),
        1,
        "late steering should trigger one compaction pass"
    );
    let seen = client.seen.lock().unwrap();
    assert_eq!(seen.len(), 1);
    assert!(
        !serde_json::to_string(&seen[0])
            .unwrap()
            .contains("during-rebuild")
    );
    assert!(serde_json::to_string(&seen[0]).unwrap().contains("summary"));
}

#[tokio::test]
async fn cancellation_during_preparation_does_not_start_the_provider() {
    let (client, _, _) = run_preparation_case(1, false, true).await;
    assert!(client.seen.lock().unwrap().is_empty());
}
