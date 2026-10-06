use super::support::*;
use super::*;
use crate::session::SessionToolOverlayPort;
use base64::Engine as _;
use haven_memory::PendingInputDisposition;
use std::sync::Mutex as StdMutex;

#[derive(Debug, PartialEq, Eq)]
enum OverlayRestoreCall {
    Unregister(String),
    Mcp(String, String, Option<Vec<String>>),
    Skill(String, Vec<String>),
    Builtin(String, Option<Vec<String>>, Option<Vec<String>>),
}

#[derive(Default)]
struct RecordingSessionToolOverlay {
    calls: StdMutex<Vec<OverlayRestoreCall>>,
}

#[async_trait]
impl SessionToolOverlayPort for RecordingSessionToolOverlay {
    async fn unregister_session(&self, session_id: &str) {
        self.calls
            .lock()
            .unwrap()
            .push(OverlayRestoreCall::Unregister(session_id.to_string()));
    }

    async fn register_mcp_for_session(
        &self,
        session_id: &str,
        server_name: &str,
        tool_names: Option<&[String]>,
    ) -> bool {
        self.calls.lock().unwrap().push(OverlayRestoreCall::Mcp(
            session_id.to_string(),
            server_name.to_string(),
            tool_names.map(|names| names.to_vec()),
        ));
        false
    }

    async fn load_skill_for_session(&self, session_id: &str, names: Vec<String>) -> bool {
        self.calls
            .lock()
            .unwrap()
            .push(OverlayRestoreCall::Skill(session_id.to_string(), names));
        false
    }

    async fn load_builtin_operations_for_session(
        &self,
        session_id: &str,
        operations: Option<Vec<String>>,
        roots: Option<Vec<String>>,
    ) -> bool {
        self.calls.lock().unwrap().push(OverlayRestoreCall::Builtin(
            session_id.to_string(),
            operations,
            roots,
        ));
        false
    }
}

fn overlay_restore_tool(tool_name: &str, tool_input: serde_json::Value) -> ToolRecord {
    ToolRecord {
        tool_call: ToolCall {
            tool_name: tool_name.to_string(),
            tool_input,
            is_final: false,
            tool_call_id: None,
        },
        observation: None,
        tool_index: 0,
        step_id: "step-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into(),
    }
}

#[tokio::test]
async fn resume_restores_tool_overlay_cleanly_in_round_order_and_best_effort() {
    let db_dir = tempfile::tempdir().unwrap();
    let db = Arc::new(Database::open(&db_dir.path().join("resume.db")).unwrap());
    let tools = Arc::new(ToolsManager::new());
    let overlay = Arc::new(RecordingSessionToolOverlay::default());
    let executor = Arc::new(SessionSupervisor::new_with_session_tool_overlay_port(
        haven_memory::SessionStore::new(db.clone()),
        tools.clone(),
        1,
        overlay.clone(),
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
        crate::AgentToolPorts::from_tools_manager(tools),
        router,
        30,
        50,
        context_limits,
    )
    .agent;
    let rounds = vec![
        ReActRound {
            step_number: 1,
            thought: None,
            tools: vec![
                overlay_restore_tool("load_mcp", serde_json::json!({"server_name": "alpha"})),
                overlay_restore_tool("load_skill", serde_json::json!({"skill_names": ["echo"]})),
                overlay_restore_tool(
                    "tool_catalog",
                    serde_json::json!({
                        "action": "load",
                        "operations": ["files.list"],
                        "roots": []
                    }),
                ),
            ],
        },
        ReActRound {
            step_number: 2,
            thought: None,
            tools: vec![overlay_restore_tool(
                "load_mcp",
                serde_json::json!({"server_name": "beta", "tool_names": []}),
            )],
        },
    ];

    agent
        .restore_per_session_tools("ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", &rounds)
        .await;

    assert_eq!(
        *overlay.calls.lock().unwrap(),
        vec![
            OverlayRestoreCall::Unregister("ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into()),
            OverlayRestoreCall::Mcp(
                "ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into(),
                "alpha".into(),
                None,
            ),
            OverlayRestoreCall::Skill(
                "ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into(),
                vec!["echo".into()],
            ),
            OverlayRestoreCall::Builtin(
                "ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into(),
                Some(vec!["files.list".into()]),
                Some(Vec::new()),
            ),
            OverlayRestoreCall::Mcp(
                "ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into(),
                "beta".into(),
                Some(Vec::new()),
            ),
        ],
        "false best-effort results must not abort the ordered replay"
    );
}

fn managed_test_image() -> (haven_common::types::MessageAttachment, std::path::PathBuf) {
    let dir = haven_common::default_work_dir()
        .join("uploads")
        .join(format!("test-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("image.png");
    let bytes = b"hello";
    std::fs::write(&path, bytes).unwrap();
    let mut attachment = haven_common::types::MessageAttachment::new(
        "image/png",
        base64::engine::general_purpose::STANDARD.encode(bytes),
    );
    attachment.asset_id = Some(haven_common::types::new_id("asset"));
    attachment.filename = Some("image.png".into());
    attachment.path = Some(path.to_string_lossy().into_owned());
    attachment.size_bytes = Some(bytes.len() as u64);
    (attachment, dir)
}

#[tokio::test]
async fn enabled_skills_are_global_and_resume_does_not_rebuild_skill_sessions() {
    // Create a skill on disk so SkillsEngine can discover it.
    let dir = std::env::temp_dir().join(format!("haven_restore_test_{}", uuid::Uuid::new_v4()));
    let skill_dir = dir.join("echo");
    std::fs::create_dir_all(skill_dir.join("scripts")).unwrap();
    std::fs::write(
        skill_dir.join("SKILL.md"),
        "# Skill: echo\n## Metadata\n- description: echo skill\n## Instructions\ndo echo\n",
    )
    .unwrap();
    std::fs::write(skill_dir.join("scripts").join("main.py"), "print('{}')\n").unwrap();

    let db = Arc::new(
        Database::open(
            &std::env::temp_dir().join(format!("haven_restore_db_{}.db", uuid::Uuid::new_v4())),
        )
        .unwrap(),
    );
    let tools = Arc::new(ToolsManager::new());
    tools
        .share_services()
        .skills
        .set_config(Some(dir.clone()), None)
        .await
        .unwrap();
    tools.rebuild_catalog().await.unwrap();
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
    let agent = Arc::new(
        AgentLayer::build(
            memory_service,
            executor,
            crate::AgentToolPorts::from_tools_manager(tools.clone()),
            router,
            30,
            50,
            context_limits,
        )
        .agent,
    );

    // Enabled skills remain host-owned deferred adapters. They are available
    // to the loader, but resume must not silently activate them in a session.
    let rounds = Vec::new();
    assert!(tools.get_tool("skill__echo").await.is_some());
    let before = tools.list_schemas_for_session("ses-x").await;
    assert!(!before.iter().any(|s| s["name"] == "skill__echo"));
    let other_before = tools.list_schemas_for_session("ses-y").await;
    assert!(!other_before.iter().any(|s| s["name"] == "skill__echo"));

    agent.restore_per_session_tools("ses-x", &rounds).await;

    // The resume path must not activate or duplicate the deferred skill.
    let after = tools.list_schemas_for_session("ses-x").await;
    assert!(!after.iter().any(|s| s["name"] == "skill__echo"));

    let other = tools.list_schemas_for_session("ses-y").await;
    assert!(!other.iter().any(|s| s["name"] == "skill__echo"));

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn reopen_session_requeues_undelivered_inputs_stays_paused() {
    let (agent, executor) = make_test_agent();
    agent.set_emitter(make_recording_emitter());
    let session = executor.create_session("input text").await.unwrap();
    // The session input (first user message) is seeded into the canonical
    // directly and never carries a step anchor.
    agent
        .persist_message_parts(&session.id, "user", "input text", Some("text"), &[], false)
        .await
        .unwrap();
    // An ordinary historical user row has no pending recovery marker.
    agent
        .persist_message_parts(
            &session.id,
            "user",
            "steering delivered",
            Some("text"),
            &[],
            false,
        )
        .await
        .unwrap();
    // A steering input lost before injection keeps a durable pending marker.
    agent
        .react_engine
        .event_store
        .persist_pending_user_input(
            &session.id,
            "steering lost",
            Some("text"),
            &[],
            false,
            None,
            PendingInputDisposition::FollowUp,
            None,
        )
        .await
        .unwrap();
    // Terminal state: the session leaves the working set (an error/cancel
    // dropped the in-memory queues along with the lost steering).
    executor
        .update_session_status(&session.id, SessionStatus::Completed)
        .await
        .unwrap();
    assert_eq!(executor.get_active_session_status(&session.id).await, None);

    agent.reopen_session(&session.id).await.unwrap();

    // Re-queued for a later Continue / follow-up, but resume stays Paused
    // so opening history never auto-runs ReAct on old chats.
    assert_eq!(
        executor.get_active_session_status(&session.id).await,
        Some(SessionStatus::Paused)
    );
    let supps = executor.get_follow_ups(&session.id).await;
    assert_eq!(supps.len(), 1, "only the never-injected input is re-queued");
    assert_eq!(supps[0].text, "steering lost");
}

#[tokio::test]
async fn reopen_session_marks_only_first_recovered_input_as_ask_answer() {
    let (agent, executor) = make_test_agent();
    let session = executor.create_session("input text").await.unwrap();
    // The initial user seed is not a recoverable supplement. Reproduce the
    // normal transcript shape so both later inputs are pending candidates.
    agent
        .persist_message_parts(&session.id, "user", "input text", Some("text"), &[], false)
        .await
        .unwrap();
    agent
        .react_engine
        .event_store
        .persist_pending_user_input(
            &session.id,
            "answer",
            Some("text"),
            &[],
            false,
            None,
            PendingInputDisposition::Answer,
            None,
        )
        .await
        .unwrap();
    agent
        .react_engine
        .event_store
        .persist_pending_user_input(
            &session.id,
            "follow-up",
            Some("text"),
            &[],
            false,
            None,
            PendingInputDisposition::FollowUp,
            None,
        )
        .await
        .unwrap();
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

    agent.reopen_session(&session.id).await.unwrap();

    let recovered = executor.get_follow_ups(&session.id).await;
    assert_eq!(recovered.len(), 2);
    assert!(recovered[0].is_answer);
    assert!(!recovered[1].is_answer);
}

#[tokio::test]
async fn reopen_preserves_follow_up_route_after_confirm_resolves_while_ask_stays_pending() {
    let db = temp_db();
    let (agent, executor) = make_test_agent_with_db(
        db.clone(),
        Arc::new(FinalAnswerMock),
        Arc::new(ToolsManager::new()),
        ContextLimitsConfig::default(),
    );
    let session = executor.create_session("input text").await.unwrap();
    agent
        .persist_message_parts(&session.id, "user", "input text", Some("text"), &[], false)
        .await
        .unwrap();
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
    let confirm = crate::interaction::InteractionRequest::confirm(
        &session.id,
        1,
        "haven.test".into(),
        serde_json::Value::Null,
        "call-test".into(),
        "step-0123456789abcdef0123456789abcdef".into(),
        0,
        haven_common::types::RiskLevel::Safe,
        Some(haven_tools::ConfirmationReceipt {
            confirmation_id: haven_common::types::new_id("conf").into(),
            capability: haven_common::types::CapabilityScope::try_new("haven.test").unwrap(),
            canonical_input_hash: String::new(),
            effective_risk: haven_common::types::RiskLevel::Safe,
            policy_revision: 1,
            expires_at: chrono::Utc::now().timestamp().max(0) as u64 + 300,
        }),
    );
    executor.request_interaction(confirm.clone()).await.unwrap();

    // Confirm has precedence when ingress freezes this route, so it is a
    // FollowUp even though an Ask is also pending.
    agent
        .process_input("accepted during confirmation", Some(session.id.clone()))
        .await
        .unwrap();
    let pending_before_restart = agent
        .react_engine
        .event_store
        .pending_session_inputs(&session.id)
        .await
        .unwrap();
    assert_eq!(pending_before_restart.len(), 1);
    assert_eq!(
        pending_before_restart[0].disposition,
        PendingInputDisposition::FollowUp
    );

    // Resolve Confirm but leave Ask pending. Reopen through a fresh executor
    // to model a process restart after the pending marker was persisted.
    executor
        .resolve_interaction(&session.id, &confirm.id, serde_json::json!(true), false)
        .await
        .unwrap()
        .expect("Confirm should resolve");
    assert_eq!(
        executor
            .pending_interactions(&session.id, crate::interaction::InteractionKind::Ask)
            .await
            .len(),
        1
    );
    executor
        .update_session_status(&session.id, SessionStatus::Completed)
        .await
        .unwrap();

    let (reopened_agent, reopened_executor) = make_test_agent_with_db(
        db,
        Arc::new(FinalAnswerMock),
        Arc::new(ToolsManager::new()),
        ContextLimitsConfig::default(),
    );
    reopened_agent.reopen_session(&session.id).await.unwrap();

    let restored = reopened_executor.get_follow_ups(&session.id).await;
    assert_eq!(restored.len(), 1);
    assert_eq!(restored[0].text, "accepted during confirmation");
    assert!(
        !restored[0].is_answer,
        "recovery must trust the saved FollowUp route"
    );
    assert_eq!(
        reopened_executor
            .pending_interactions(&session.id, crate::interaction::InteractionKind::Ask)
            .await
            .len(),
        1,
        "the Ask remains pending after Confirm resolution and reopen"
    );
}

#[tokio::test]
async fn reopen_session_without_pending_inputs_stays_paused() {
    let (agent, executor) = make_test_agent();
    let session = executor.create_session("input text").await.unwrap();
    agent
        .persist_message_parts(&session.id, "user", "input text", Some("text"), &[], false)
        .await
        .unwrap();
    executor
        .update_session_status(&session.id, SessionStatus::Completed)
        .await
        .unwrap();

    agent.reopen_session(&session.id).await.unwrap();

    // No lost inputs: the session reopens as Paused (resume-only).
    assert_eq!(
        executor.get_active_session_status(&session.id).await,
        Some(SessionStatus::Paused)
    );
    assert!(executor.get_follow_ups(&session.id).await.is_empty());
}

#[tokio::test]
async fn resume_dedups_supplement_inputs_against_prefixed_canonical() {
    // Follow-up/steering inputs are pushed into the canonical with a
    // text prefix ("Additional context from user: —, "Steering: —)
    // while the DB stores the raw text. This historical fixture has no
    // pending marker, so it is not re-injected as a fresh user turn.
    let (agent, executor) = make_test_agent();
    agent.set_emitter(make_recording_emitter());
    let session = executor.create_session("hello").await.unwrap();
    // DB stores the RAW user text (this is what process_input persists).
    agent
        .persist_message_parts(
            &session.id,
            "user",
            "please be brief",
            Some("text"),
            &[],
            false,
        )
        .await
        .unwrap();
    // Canonical carries the prefixed form (as push_user_context emits it).
    let canonical = vec![
        CanonicalMessage::system(vec![ContentPart::text("sys")]),
        CanonicalMessage::user_text("hello"),
        CanonicalMessage::user_text("Additional context from user: please be brief"),
        CanonicalMessage::user_text("Steering: please be brief"),
    ];
    let snapshot = EventProjection {
        events: seed_events_from_canonical(canonical),
        step_number: 1,
        branch_points: HashMap::new(),
        interactions: Vec::new(),
    };
    seed_event_projection(&agent, &session.id, &snapshot).await;
    agent.run_session_from_id(&session.id).await.unwrap();

    let saved = load_event_projection(&agent, &session.id).await;
    let (canonical, _) = saved.project();
    let user_texts: Vec<String> = canonical
        .iter()
        .filter(|m| m.role == CanonicalRole::User)
        .filter_map(|m| {
            m.content.iter().find_map(|p| match p {
                ContentPart::Text(t) => Some(t.clone()),
                _ => None,
            })
        })
        .collect();
    assert_eq!(
        user_texts
            .iter()
            .filter(|t| t.as_str() == "[conversation] [user] please be brief")
            .count(),
        0,
        "supplement text already present (prefixed) must not be re-seeded: {:?}",
        user_texts
    );
}

#[tokio::test]
async fn resume_keeps_repeated_same_text_turns() {
    // Two distinct turns with identical text (user said "好的" twice) are
    // both legitimate history. The durable event stream is the authority for
    // everything it contains; the second input remains pending by message id,
    // so identical text is recovered too.
    let (agent, executor) = make_test_agent();
    agent.set_emitter(make_recording_emitter());
    let session = executor.create_session("hello").await.unwrap();
    agent
        .persist_message_parts(&session.id, "user", "好的", Some("text"), &[], false)
        .await
        .unwrap();
    agent
        .persist_message_parts(&session.id, "assistant", "好的", Some("text"), &[], false)
        .await
        .unwrap();
    // The first pair is already in the event log. The second identical user
    // turn is persisted afterward and remains pending by id.
    let canonical = vec![
        CanonicalMessage::system(vec![ContentPart::text("sys")]),
        CanonicalMessage::user_text("好的"),
        CanonicalMessage::assistant(
            vec![ContentPart::text("好的")],
            None,
            None,
            Vec::new(),
            Vec::new(),
        ),
    ];
    let snapshot = EventProjection {
        events: seed_events_from_canonical(canonical),
        step_number: 1,
        branch_points: HashMap::new(),
        interactions: Vec::new(),
    };
    seed_event_projection(&agent, &session.id, &snapshot).await;
    // The second identical user turn remains explicitly pending after the
    // snapshot, even though its text matches the earlier turn.
    agent
        .react_engine
        .event_store
        .persist_pending_user_input(
            &session.id,
            "好的",
            Some("text"),
            &[],
            false,
            None,
            PendingInputDisposition::FollowUp,
            None,
        )
        .await
        .unwrap();

    agent.run_session_from_id(&session.id).await.unwrap();
    assert!(
        agent
            .react_engine
            .event_store
            .pending_session_inputs(&session.id)
            .await
            .unwrap()
            .is_empty()
    );

    let saved = load_event_projection(&agent, &session.id).await;
    let (canonical, _) = saved.project();
    let user_texts: Vec<String> = canonical
        .iter()
        .filter(|m| m.role == CanonicalRole::User)
        .filter_map(|m| {
            m.content.iter().find_map(|p| match p {
                ContentPart::Text(t) => Some(t.clone()),
                _ => None,
            })
        })
        .collect();
    // B3: UserInject stores raw text; wire prefixes are adapter-only.
    // Both turns project as plain "好的" — distinguish via InjectSource.
    let follow_ups: Vec<_> = canonical
        .iter()
        .filter(|m| m.role == CanonicalRole::User && m.source == Some(InjectSource::FollowUp))
        .collect();
    assert_eq!(
        user_texts.iter().filter(|t| t.as_str() == "好的").count(),
        2,
        "both identical user turns must appear as raw text: {:?}",
        user_texts
    );
    assert_eq!(
        follow_ups.len(),
        1,
        "the second identical user turn must be recovered by ingress id as FollowUp: {:?}",
        user_texts
    );
    assert!(
        user_texts.iter().all(|t| !t.starts_with("[conversation] ")),
        "no [conversation]-wrapped lines may exist: {:?}",
        user_texts
    );
}

#[tokio::test]
async fn resume_does_not_recover_unmarked_historical_user_messages() {
    // Historical message rows that do not carry a pending-input marker are
    // already represented by the event authority and must not be re-queued.
    let (agent, executor) = make_test_agent();
    agent.set_emitter(make_recording_emitter());
    let session = executor.create_session("hello").await.unwrap();
    agent
        .persist_message_parts(&session.id, "user", "hello", Some("text"), &[], false)
        .await
        .unwrap();
    let canonical = vec![
        CanonicalMessage::system(vec![ContentPart::text("sys")]),
        CanonicalMessage::user_text("hello"),
        CanonicalMessage::assistant(
            vec![ContentPart::text("hi there")],
            None,
            None,
            Vec::new(),
            Vec::new(),
        ),
    ];
    let snapshot = EventProjection {
        events: seed_events_from_canonical(canonical),
        step_number: 1,
        branch_points: HashMap::new(),
        interactions: Vec::new(),
    };
    seed_event_projection(&agent, &session.id, &snapshot).await;
    // Assistant rows are not pending inputs and are never recovered.
    agent
        .persist_message_parts(
            &session.id,
            "assistant",
            "hi there",
            Some("text"),
            &[],
            false,
        )
        .await
        .unwrap();

    agent.run_session_from_id(&session.id).await.unwrap();

    let saved = load_event_projection(&agent, &session.id).await;
    let (canonical, _) = saved.project();
    let user_texts: Vec<String> = canonical
        .iter()
        .filter(|m| m.role == CanonicalRole::User)
        .filter_map(|m| {
            m.content.iter().find_map(|p| match p {
                ContentPart::Text(t) => Some(t.clone()),
                _ => None,
            })
        })
        .collect();
    assert_eq!(
        user_texts.iter().filter(|t| t.as_str() == "hello").count(),
        1,
        "unmarked historical user input must not be recovered: {:?}",
        user_texts
    );
    assert!(
        user_texts
            .iter()
            .all(|t| !t.starts_with("Additional context from user:")),
        "no unmarked supplement may appear: {:?}",
        user_texts
    );
}

#[tokio::test]
async fn resume_skips_conversation_reseed_when_canonical_is_compacted() {
    // Compaction replaces the old turns with a summary inside the canonical
    // but leaves the DB message stream untouched. Those historical rows have
    // no pending markers, so they are never resurrected and the compacted
    // canonical stays compacted across resume.
    let (agent, executor) = make_test_agent();
    agent.set_emitter(make_recording_emitter());
    let session = executor.create_session("hello").await.unwrap();
    agent
        .persist_message_parts(&session.id, "user", "hello", Some("text"), &[], false)
        .await
        .unwrap();
    agent
        .persist_message_parts(
            &session.id,
            "assistant",
            "long ago answer",
            Some("text"),
            &[],
            false,
        )
        .await
        .unwrap();
    let canonical = vec![
        CanonicalMessage::system(vec![ContentPart::text("sys")]),
        CanonicalMessage::assistant(
            vec![ContentPart::text(
                "[Compacted summary of previous messages]: hello / long ago answer",
            )],
            None,
            None,
            Vec::new(),
            Vec::new(),
        ),
    ];
    let snapshot = EventProjection {
        events: seed_events_from_canonical(canonical),
        step_number: 1,
        branch_points: HashMap::new(),
        interactions: Vec::new(),
    };
    seed_event_projection(&agent, &session.id, &snapshot).await;

    agent.run_session_from_id(&session.id).await.unwrap();

    let saved = load_event_projection(&agent, &session.id).await;
    let (canonical, _) = saved.project();
    let user_texts: Vec<String> = canonical
        .iter()
        .filter(|m| m.role == CanonicalRole::User)
        .filter_map(|m| {
            m.content.iter().find_map(|p| match p {
                ContentPart::Text(t) => Some(t.clone()),
                _ => None,
            })
        })
        .collect();
    assert!(
        user_texts.iter().all(|t| !t.starts_with("[conversation] ")),
        "compacted canonical must not be re-seeded from the DB window: {:?}",
        user_texts
    );
}

#[tokio::test]
async fn run_session_from_id_keeps_first_user_media_out_of_snapshot_bytes() {
    let (agent, executor) = make_test_agent();
    agent.set_emitter(make_recording_emitter());
    let session = executor
        .create_session_with_summary("看图", "看图")
        .await
        .unwrap();
    let (att, asset_dir) = managed_test_image();
    agent
        .persist_message_parts(&session.id, "user", "看图", Some("text"), &[att], false)
        .await
        .unwrap();
    agent.run_session_from_id(&session.id).await.unwrap();
    let snapshot = load_event_projection(&agent, &session.id).await;
    let (canonical, _) = snapshot.project();
    let user_msg = canonical
        .iter()
        .find(|m| m.role == CanonicalRole::User)
        .expect("initial user message exists");
    assert!(user_msg.content.iter().any(|p| matches!(
        p,
        ContentPart::Text(text) if text.contains("managed image omitted from snapshot")
    )));
    let _ = std::fs::remove_dir_all(asset_dir);
}

#[tokio::test]
async fn run_session_from_id_recovers_pending_input_without_event_log() {
    let (agent, executor) = make_test_agent();
    agent.set_emitter(make_recording_emitter());
    let session = executor
        .create_session_with_summary("initial", "initial")
        .await
        .unwrap();
    agent
        .persist_message_parts(&session.id, "user", "initial", Some("text"), &[], false)
        .await
        .unwrap();
    agent
        .react_engine
        .event_store
        .persist_pending_user_input(
            &session.id,
            "accepted before first run",
            Some("text"),
            &[],
            false,
            None,
            PendingInputDisposition::FollowUp,
            None,
        )
        .await
        .unwrap();

    // No event log exists yet, so startup takes the fresh-run path. The
    // durable marker must still restore the accepted supplement.
    agent.run_session_from_id(&session.id).await.unwrap();

    assert!(
        agent
            .react_engine
            .event_store
            .pending_session_inputs(&session.id)
            .await
            .unwrap()
            .is_empty()
    );
    let saved = load_event_projection(&agent, &session.id).await;
    let (canonical, _) = saved.project();
    assert!(canonical.iter().any(|message| {
        message.role == CanonicalRole::User
            && message.source == Some(InjectSource::FollowUp)
            && message.content.iter().any(|part| {
                matches!(part, ContentPart::Text(text) if text == "accepted before first run")
            })
    }));
}

#[tokio::test]
async fn run_session_from_id_keeps_later_media_as_managed_reference() {
    let (agent, executor) = make_test_agent();
    agent.set_emitter(make_recording_emitter());
    let session = executor
        .create_session_with_summary("plain session", "plain session")
        .await
        .unwrap();
    agent
        .persist_message_parts(
            &session.id,
            "user",
            "plain session",
            Some("text"),
            &[],
            false,
        )
        .await
        .unwrap();
    // Image arrives AFTER the session input (a supplement) ??it must not be
    // attached to the initial user turn.
    let (att, asset_dir) = managed_test_image();
    agent
        .process_input_with_attachments("补充看图", Some(session.id.clone()), &[att], false)
        .await
        .unwrap();
    agent.run_session_from_id(&session.id).await.unwrap();
    let snapshot = load_event_projection(&agent, &session.id).await;
    let (canonical, _) = snapshot.project();
    let first_user = canonical
        .iter()
        .find(|m| m.role == CanonicalRole::User)
        .expect("initial user message exists");
    assert!(
        !first_user
            .content
            .iter()
            .any(|p| matches!(p, ContentPart::Image { .. })),
        "image supplement must not be attached to the initial user turn"
    );
    // The supplement itself is still injected later, but the snapshot carries
    // only the opaque managed reference rather than inline image bytes.
    assert!(
        canonical.iter().any(|m| m
            .content
            .iter()
            .any(|p| matches!(p, ContentPart::Text(text) if text.contains("asset_id=")))),
        "supplement media should be injected as a managed reference"
    );
    let _ = std::fs::remove_dir_all(asset_dir);
}

#[tokio::test]
async fn run_session_from_id_trims_dangling_tool_call_before_resume() {
    // Simulate a snapshot saved by save_branch_point right after the
    // assistant tool_call message but before tool results were appended
    // (e.g. the app was closed mid-tool-execution). Resuming must trim
    // the dangling assistant message instead of sending it to the LLM,
    // which would reject it with a 400 error.
    let tools = Arc::new(ToolsManager::new());
    tools
        .registry()
        .register(Arc::new(EchoTool) as ToolBox)
        .await
        .unwrap();
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

    let canonical = vec![
        CanonicalMessage {
            role: CanonicalRole::System,
            content: vec![ContentPart::text("system prompt")],
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
            content: vec![ContentPart::text("resume me")],
            tool_calls: None,
            tool_call_id: None,
            reasoning: None,
            web_search_calls: Vec::new(),
            thinking_blocks: Vec::new(),
            source: None,
            id: None,
        },
        CanonicalMessage {
            role: CanonicalRole::Assistant,
            content: vec![ContentPart::text("calling echo")],
            tool_calls: Some(vec![CanonicalToolCall {
                id: "call_1".into(),
                name: "echo".into(),
                arguments: serde_json::json!({"text": "hi"}),
            }]),
            tool_call_id: None,
            reasoning: None,
            web_search_calls: Vec::new(),
            thinking_blocks: Vec::new(),
            source: None,
            id: None,
        },
    ];
    // Thought-only round for the dangling assistant tool_call (no ToolResult yet).
    let events = {
        let mut e = seed_events_from_canonical(canonical);
        e.push(TranscriptRecord::Thought {
            step_number: 1,
            text: "calling echo".into(),
            message_id: "step-1".into(),
        });
        e
    };
    let snapshot = EventProjection {
        events,
        step_number: 2,
        branch_points: HashMap::new(),
        interactions: Vec::new(),
    };
    seed_event_projection(&agent, &session.id, &snapshot).await;

    let result = agent.run_session_from_id(&session.id).await.unwrap();

    // No batch sent to the LLM may end with a dangling assistant tool_call.
    {
        let seen = mock.seen.lock().unwrap();
        assert!(!seen.is_empty(), "LLM should have been called after resume");
        for batch in seen.iter() {
            let last = batch.last().expect("batch has messages");
            assert!(
                !(matches!(last.role, CanonicalRole::Assistant) && last.tool_calls.is_some()),
                "batch must not end with a dangling assistant tool_call: {:?}",
                batch
            );
        }
    }
    assert!(!result.is_empty(), "resumed loop should produce history");
    assert_eq!(
        executor.get_active_session_status(&session.id).await,
        Some(SessionStatus::Paused),
        "final_answer should complete the resumed session"
    );
}
