use super::support::*;
use super::*;

#[tokio::test]
async fn restore_per_session_tools_rebuilds_from_history() {
    // Create a skill on disk so SkillsEngine can discover it.
    let dir = std::env::temp_dir().join(format!("haven_restore_test_{}", uuid::Uuid::new_v4()));
    let skill_dir = dir.join("echo");
    std::fs::create_dir_all(skill_dir.join("scripts")).unwrap();
    std::fs::write(
        skill_dir.join("SKILL.md"),
        "# Skill: echo\n## Metadata\n- description: echo skill\n## Instructions\ndo echo\n",
    )
    .unwrap();

    let db = Arc::new(
        Database::open(
            &std::env::temp_dir().join(format!("haven_restore_db_{}.db", uuid::Uuid::new_v4())),
        )
        .unwrap(),
    );
    let tools = Arc::new(ToolsManager::new());
    tools
        .skills_engine
        .set_config(Some(dir.clone()), None)
        .await
        .unwrap();
    tools.rebuild_catalog().await;
    let executor = Arc::new(SessionExecutor::new(db.clone(), tools.clone(), 1));
    let client = Arc::new(FinalAnswerMock) as Arc<dyn LlmClient>;
    let router = Arc::new(LlmRouter::new_with_clients(
        client.clone(),
        client.clone(),
        client.clone(),
        client,
    ));
    let agent = Arc::new(AgentLayer::new(
        db,
        executor,
        router,
        30,
        50,
        ContextLimitsConfig::default(),
    ));

    // Simulate rounds where load_skill was called.
    let rounds = vec![ReActRound {
        step_number: 1,
        thought: Some("I need the echo skill".into()),
        tools: vec![ToolRecord {
            action: Action {
                tool_name: "load_skill".into(),
                tool_input: serde_json::json!({"skill_name": "echo"}),
                is_final: false,
                tool_call_id: Some("tc1".into()),
            },
            observation: Some(r#"{"skill":{"name":"skill__echo"}}"#.into()),
            action_index: 0,
            step_id: "step-skill".into(),
        }],
    }];

    // Before restore, no per-session tools.
    let before = tools.list_schemas_for_session("ses-x").await;
    assert!(!before.iter().any(|s| s["name"] == "skill__echo"));

    agent.restore_per_session_tools("ses-x", &rounds).await;

    // After restore, the skill tool should be visible per-session.
    let after = tools.list_schemas_for_session("ses-x").await;
    assert!(
        after.iter().any(|s| s["name"] == "skill__echo"),
        "restored skill should appear in per-session schemas"
    );

    // Other sessions should NOT see it.
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
    // A steering input that WAS delivered carries a step anchor under its
    // own id (created by `push_user_context`).
    let delivered = agent
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
    agent
        .db
        .create_thought_step(&session.id, 2, &delivered.id)
        .unwrap();
    // A steering input lost before injection has no anchor.
    agent
        .persist_message_parts(
            &session.id,
            "user",
            "steering lost",
            Some("text"),
            &[],
            false,
        )
        .await
        .unwrap();
    // Terminal state: the session leaves the working set (an error/cancel
    // dropped the in-memory queues along with the lost steering).
    executor
        .update_session_status(&session.id, SessionStatus::Completed)
        .await
        .unwrap();
    assert_eq!(executor.get_session_state(&session.id).await, None);

    agent.reopen_session(&session.id).await.unwrap();

    // Re-queued for a later Continue / follow-up, but resume stays Paused
    // so opening history never auto-runs ReAct on old chats.
    assert_eq!(
        executor.get_session_state(&session.id).await,
        Some(SessionStatus::Paused)
    );
    let supps = executor.get_supplements(&session.id).await;
    assert_eq!(supps.len(), 1, "only the never-injected input is re-queued");
    assert_eq!(supps[0].text, "steering lost");
}

#[tokio::test]
async fn reopen_session_marks_only_first_recovered_input_as_ask_answer() {
    let (agent, executor) = make_test_agent();
    let session = executor.create_session("input text").await.unwrap();
    // The initial user seed is not a recoverable supplement. Reproduce the
    // normal transcript shape so both later unanchored inputs are candidates.
    agent
        .persist_message_parts(&session.id, "user", "input text", Some("text"), &[], false)
        .await
        .unwrap();
    agent
        .persist_message_parts(&session.id, "user", "answer", Some("text"), &[], false)
        .await
        .unwrap();
    agent
        .persist_message_parts(&session.id, "user", "follow-up", Some("text"), &[], false)
        .await
        .unwrap();
    executor
        .update_session_status(&session.id, SessionStatus::PausedAwaitingAnswer)
        .await
        .unwrap();

    agent.reopen_session(&session.id).await.unwrap();

    let recovered = executor.get_supplements(&session.id).await;
    assert_eq!(recovered.len(), 2);
    assert!(recovered[0].is_answer);
    assert!(!recovered[1].is_answer);
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

    // No lost inputs: the session reopens as Paused (resume-only),
    // matching the historical behavior.
    assert_eq!(
        executor.get_session_state(&session.id).await,
        Some(SessionStatus::Paused)
    );
    assert!(executor.get_supplements(&session.id).await.is_empty());
}

#[tokio::test]
async fn resume_rejects_legacy_conversation_prefix_snapshot() {
    // A snapshot carrying the old `[conversation]` seed is incompatible with
    // the current events-authority format and must require a reset.
    let (agent, executor) = make_test_agent();
    let session = executor.create_session("hello").await.unwrap();
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
        CanonicalMessage::user_text("[conversation] [user] hello"),
        CanonicalMessage::user_text("[conversation] [assistant] hi there"),
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
    };
    agent
        .db
        .save_react_state(&session.id, &serde_json::to_string(&snapshot).unwrap())
        .unwrap();

    let err = agent.run_session_from_id(&session.id).await.unwrap_err();
    assert!(err.to_string().contains("incompatible"));
}

#[tokio::test]
async fn resume_dedups_supplement_inputs_against_prefixed_canonical() {
    // Supplement/steering inputs are pushed into the canonical with a
    // text prefix ("Additional context from user: —, "Steering: —)
    // while the DB stores the raw text. A legacy snapshot (no saved_at)
    // is trusted as complete: nothing is recovered, so the already
    // prefixed inputs are never re-injected as fresh user turns.
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
    let snapshot = ReActSnapshot {
        events: seed_events_from_canonical(canonical),
        step_number: 1,
        branch_points: HashMap::new(),
        saved_at: None,
        awaiting_answer: None,
        awaiting_confirm: None,
        run_budget: None,
        error_partial_message_ids: None,
    };
    agent
        .db
        .save_react_state(&session.id, &serde_json::to_string(&snapshot).unwrap())
        .unwrap();

    agent.run_session_from_id(&session.id).await.unwrap();

    let saved: ReActSnapshot =
        serde_json::from_str(&agent.db.get_react_state(&session.id).unwrap().unwrap()).unwrap();
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
    // both legitimate history. The snapshot is the single authority for
    // everything it contains; a message persisted AFTER the snapshot's
    // saved_at is recovered by timestamp — identical text is recovered
    // too (timestamp recovery never drops a repeated turn).
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
    // Snapshot saved right after the first pair: its saved_at sits
    // between the persisted rows and the second user "好的" below.
    let msgs_before = agent.db.get_session_messages(&session.id).unwrap();
    let saved_at = msgs_before[1].created_at.clone();
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
    let snapshot = ReActSnapshot {
        events: seed_events_from_canonical(canonical),
        step_number: 1,
        branch_points: HashMap::new(),
        saved_at: Some(saved_at),
        awaiting_answer: None,
        awaiting_confirm: None,
        run_budget: None,
        error_partial_message_ids: None,
    };
    agent
        .db
        .save_react_state(&session.id, &serde_json::to_string(&snapshot).unwrap())
        .unwrap();
    // The second identical user turn lands after the snapshot.
    agent
        .persist_message_parts(&session.id, "user", "好的", Some("text"), &[], false)
        .await
        .unwrap();

    agent.run_session_from_id(&session.id).await.unwrap();

    let saved: ReActSnapshot =
        serde_json::from_str(&agent.db.get_react_state(&session.id).unwrap().unwrap()).unwrap();
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
        "the second identical user turn must be recovered by timestamp as FollowUp: {:?}",
        user_texts
    );
    assert!(
        user_texts.iter().all(|t| !t.starts_with("[conversation] ")),
        "no [conversation]-wrapped lines may exist: {:?}",
        user_texts
    );
}

#[tokio::test]
async fn resume_does_not_recover_messages_before_saved_at() {
    // Timestamp recovery is bounded by the snapshot's saved_at: rows
    // persisted before it are already represented in the canonical and
    // must NOT be re-queued, even when the canonical never carried them
    // as user turns (e.g. an ask question persisted under the step id).
    let (agent, executor) = make_test_agent();
    agent.set_emitter(make_recording_emitter());
    let session = executor.create_session("hello").await.unwrap();
    agent
        .persist_message_parts(&session.id, "user", "hello", Some("text"), &[], false)
        .await
        .unwrap();
    let saved_at = agent.db.get_session_messages(&session.id).unwrap()[0]
        .created_at
        .clone();
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
    let snapshot = ReActSnapshot {
        events: seed_events_from_canonical(canonical),
        step_number: 1,
        branch_points: HashMap::new(),
        saved_at: Some(saved_at),
        awaiting_answer: None,
        awaiting_confirm: None,
        run_budget: None,
        error_partial_message_ids: None,
    };
    agent
        .db
        .save_react_state(&session.id, &serde_json::to_string(&snapshot).unwrap())
        .unwrap();
    // Assistant rows older than saved_at are not recovered either.
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

    let saved: ReActSnapshot =
        serde_json::from_str(&agent.db.get_react_state(&session.id).unwrap().unwrap()).unwrap();
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
        "nothing older than saved_at may be recovered: {:?}",
        user_texts
    );
    assert!(
        user_texts
            .iter()
            .all(|t| !t.starts_with("Additional context from user:")),
        "no post-snapshot supplement may appear: {:?}",
        user_texts
    );
}

#[tokio::test]
async fn resume_skips_conversation_reseed_when_canonical_is_compacted() {
    // Compaction replaces the old turns with a summary inside the
    // canonical but leaves the DB message stream untouched. Recovery is
    // timestamp-bounded (only rows newer than the snapshot's saved_at are
    // re-queued), so the summarized-away turns — all older than the
    // snapshot — are never resurrected; a compacted canonical stays
    // compacted across resume.
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
    let snapshot = ReActSnapshot {
        events: seed_events_from_canonical(canonical),
        step_number: 1,
        branch_points: HashMap::new(),
        saved_at: None,
        awaiting_answer: None,
        awaiting_confirm: None,
        run_budget: None,
        error_partial_message_ids: None,
    };
    agent
        .db
        .save_react_state(&session.id, &serde_json::to_string(&snapshot).unwrap())
        .unwrap();

    agent.run_session_from_id(&session.id).await.unwrap();

    let saved: ReActSnapshot =
        serde_json::from_str(&agent.db.get_react_state(&session.id).unwrap().unwrap()).unwrap();
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
async fn run_session_from_id_attaches_first_user_message_images() {
    let (agent, executor) = make_test_agent();
    agent.set_emitter(make_recording_emitter());
    let session = executor
        .create_session_with_summary("看图", "看图")
        .await
        .unwrap();
    let att = haven_common::types::MessageAttachment::new("image/png", "aGVsbG8=");
    agent
        .persist_message_parts(&session.id, "user", "看图", Some("text"), &[att], false)
        .await
        .unwrap();
    agent.run_session_from_id(&session.id).await.unwrap();
    let snapshot: crate::types::ReActSnapshot =
        serde_json::from_str(&agent.db.get_react_state(&session.id).unwrap().unwrap()).unwrap();
    let (canonical, _) = snapshot.project();
    let user_msg = canonical
        .iter()
        .find(|m| m.role == CanonicalRole::User)
        .expect("initial user message exists");
    assert!(
        user_msg
            .content
            .iter()
            .any(|p| matches!(p, ContentPart::Image { .. })),
        "initial user message should carry the image part"
    );
}

#[tokio::test]
async fn run_session_from_id_ignores_later_image_supplement() {
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
    let att = haven_common::types::MessageAttachment::new("image/png", "aGVsbG8=");
    agent
        .process_input_with_attachments("补充看图", Some(session.id.clone()), &[att], false)
        .await
        .unwrap();
    agent.run_session_from_id(&session.id).await.unwrap();
    let snapshot: crate::types::ReActSnapshot =
        serde_json::from_str(&agent.db.get_react_state(&session.id).unwrap().unwrap()).unwrap();
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
    // The supplement itself is still injected (with its image) later.
    assert!(
        canonical.iter().any(|m| m
            .content
            .iter()
            .any(|p| matches!(p, ContentPart::Image { .. }))),
        "supplement image should be injected into the conversation"
    );
}

#[tokio::test]
async fn run_session_from_id_trims_dangling_tool_call_before_resume() {
    // Simulate a snapshot saved by save_branch_point right after the
    // assistant tool_call message but before tool results were appended
    // (e.g. the app was closed mid-tool-execution). Resuming must trim
    // the dangling assistant message instead of sending it to the LLM,
    // which would reject it with a 400 error.
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
    let snapshot = ReActSnapshot {
        events,
        step_number: 2,
        branch_points: HashMap::new(),
        saved_at: None,
        awaiting_answer: None,
        awaiting_confirm: None,
        run_budget: None,
        error_partial_message_ids: None,
    };
    agent
        .db
        .save_react_state(&session.id, &serde_json::to_string(&snapshot).unwrap())
        .unwrap();

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
        executor.get_session_state(&session.id).await,
        Some(SessionStatus::Paused),
        "final_answer should complete the resumed session"
    );
}

/// Phase 7 / B4: corrupt react_state must hard-fail, not silently fork
/// into the snapshot-less projector path.
#[tokio::test]
async fn corrupt_react_state_hard_fails_resume() {
    let tools = Arc::new(ToolsManager::new());
    let mock = Arc::new(ScriptedMock::new(vec![ScriptedResponse::Chunk(
        StreamChunk {
            text: Some("should not run".into()),
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
    agent.set_emitter(collector);
    let session = executor.create_session("corrupt snap").await.unwrap();
    agent
        .db
        .save_react_state(&session.id, "{not-valid-json")
        .unwrap();

    let err = agent
        .run_session_from_id(&session.id)
        .await
        .expect_err("corrupt snapshot must hard-fail");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("corrupt") || msg.contains("incompatible"),
        "error should mention corrupt/incompatible snapshot: {msg}"
    );
    assert!(
        mock.seen.lock().unwrap().is_empty(),
        "LLM must not be called after corrupt-snapshot hard-fail"
    );
}

/// Phase 7 / B4: projector uses the provider tool-call id persisted on the
/// step row, without matching tool-role content.
#[tokio::test]
async fn project_tool_chain_uses_step_tool_call_id() {
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
    agent.set_emitter(collector);
    let session = executor.create_session("real call id").await.unwrap();
    agent
        .persist_message_parts(
            &session.id,
            "user",
            "real call id",
            Some("text"),
            &[],
            false,
        )
        .await
        .unwrap();
    agent
        .db
        .run_blocking({
            let session_id = session.id.clone();
            move |db| {
                let step = db.create_action_step_with_identity(
                    &session_id,
                    1,
                    0,
                    "echo",
                    r#"{"text":"hi"}"#,
                    Some("call_real_1"),
                    false,
                    false,
                    None,
                    None,
                )?;
                db.complete_action_step(&step.id, "hi", true)?;
                db.add_message(
                    &session_id,
                    "tool",
                    "hi",
                    Some("observation"),
                    Some("call_real_1"),
                )?;
                Ok::<(), anyhow::Error>(())
            }
        })
        .await
        .unwrap();
    assert!(agent.db.get_react_state(&session.id).unwrap().is_none());

    agent.run_session_from_id(&session.id).await.unwrap();

    let seen = mock.seen.lock().unwrap();
    let first = &seen[0];
    let used_real = first.iter().any(|m| {
        matches!(m.role, CanonicalRole::Assistant)
            && m.tool_calls.as_ref().is_some_and(|c| {
                c.iter()
                    .any(|tc| tc.name == "echo" && tc.id == "call_real_1")
            })
    });
    assert!(
        used_real,
        "projector must reuse the tool_call_id persisted on session_steps"
    );
}
