use super::support::*;
use super::*;

#[tokio::test]
async fn rollback_without_react_state_truncates_messages() {
    let (agent, executor) = make_test_agent();
    let session = executor.create_session("no state").await.unwrap();
    // No react_state saved ??simulate an old session that errored before
    // snapshots were persisted.
    agent
        .db
        .update_session_status(&session.id, "error")
        .unwrap();
    executor
        .update_session_status(&session.id, SessionStatus::Error)
        .await
        .unwrap();
    agent
        .db
        .add_message(&session.id, "user", "hello", Some("text"), None)
        .unwrap();
    agent
        .db
        .add_message(&session.id, "assistant", "partial", Some("text"), None)
        .unwrap();
    let hello_id = agent
        .db
        .get_session_messages(&session.id)
        .unwrap()
        .into_iter()
        .find(|m| m.content == "hello")
        .unwrap()
        .id;

    // User-message rollback (pause=true) should truncate from the user msg.
    agent
        .rollback_session(&session.id, 1, true, Some(&hello_id))
        .await
        .unwrap();
    assert_eq!(
        executor.get_session_state(&session.id).await,
        Some(SessionStatus::Paused)
    );
    let msgs = agent.db.get_session_messages(&session.id).unwrap();
    assert!(msgs.is_empty(), "messages should be empty after rollback");
}

#[tokio::test]
async fn rollback_with_snapshot_no_branch_point_uses_snapshot() {
    let (agent, executor) = make_test_agent();
    let session = executor.create_session("no bp").await.unwrap();
    // Save a snapshot with no branch_points at the target step.
    let canonical = vec![CanonicalMessage {
        role: CanonicalRole::System,
        content: vec![ContentPart::text("sys")],
        tool_calls: None,
        tool_call_id: None,
        reasoning: None,
        web_search_calls: Vec::new(),
        thinking_blocks: Vec::new(),
        source: None,
        id: None,
    }];
    let snapshot = ReActSnapshot {
        events: seed_events_from_canonical(canonical.clone()),
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
    agent
        .db
        .add_message(&session.id, "user", "hello", Some("text"), None)
        .unwrap();
    // The partial must land strictly after the user row (rollback
    // truncates rows created_at > the snapshot's last message ts).
    std::thread::sleep(std::time::Duration::from_millis(5));
    agent
        .db
        .add_message(&session.id, "assistant", "partial", Some("text"), None)
        .unwrap();

    // Rollback to step 1 with pause=false (agent rollback).
    agent
        .rollback_session(&session.id, 1, false, None)
        .await
        .unwrap();
    assert_eq!(
        executor.get_session_state(&session.id).await,
        Some(SessionStatus::Pending)
    );
    // The partial assistant message should be deleted, user message kept.
    let msgs = agent.db.get_session_messages(&session.id).unwrap();
    assert_eq!(msgs.len(), 1);
    assert_eq!(msgs[0].content, "hello");
}

#[tokio::test]
async fn rollback_pause_true_removes_user_message_from_session() {
    let (agent, executor) = make_test_agent();
    let session = executor.create_session("user rollback").await.unwrap();
    agent
        .db
        .add_message(&session.id, "user", "hello", Some("text"), None)
        .unwrap();
    agent
        .db
        .add_message(&session.id, "assistant", "thinking", Some("text"), None)
        .unwrap();
    // Branch point at step 1: canonical ends at the user message, but
    // last_msg_at points at the thought that was persisted AFTER it (the
    // realistic shape saved by save_branch_point).
    let msgs = agent.db.get_session_messages(&session.id).unwrap();
    let hello_id = msgs
        .iter()
        .find(|m| m.content == "hello")
        .unwrap()
        .id
        .clone();
    let thought_ts = msgs
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
            last_msg_at: Some(thought_ts),
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
        upgrade_tool_rounds: Vec::new(),
    };
    agent
        .db
        .save_react_state(&session.id, &serde_json::to_string(&snapshot).unwrap())
        .unwrap();

    // User-message rollback: the user message itself must be removed from
    // the session (its text returns to the composer for editing) ??not
    // left behind to reappear on the next resume rebuild.
    agent
        .rollback_session(&session.id, 1, true, Some(&hello_id))
        .await
        .unwrap();
    assert_eq!(
        executor.get_session_state(&session.id).await,
        Some(SessionStatus::Paused)
    );
    let msgs = agent.db.get_session_messages(&session.id).unwrap();
    assert!(
        msgs.is_empty(),
        "user message should be deleted from the session, got {:?}",
        msgs.iter().map(|m| &m.content).collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn rollback_fallback_no_branch_point_pause_true_deletes_from_last_user_message() {
    // Regression: rollback to a step that has NO branch point (e.g. the
    // step failed before save_branch_point ran) falls back to a cutoff
    // derived from session messages. With pause=true the user message
    // itself must be removed too — and because the clicked message's
    // live-view id never matches a DB id, the backend can only guess the
    // target from the newest user message at/before the cutoff.
    let (agent, executor) = make_test_agent();
    let session = executor
        .create_session("fallback user rollback")
        .await
        .unwrap();
    agent
        .db
        .add_message(&session.id, "user", "first", Some("text"), None)
        .unwrap();
    agent
        .db
        .add_message(&session.id, "assistant", "reply1", Some("text"), None)
        .unwrap();
    agent
        .db
        .add_message(&session.id, "user", "second", Some("text"), None)
        .unwrap();
    agent
        .db
        .add_message(&session.id, "assistant", "reply2", Some("text"), None)
        .unwrap();
    let msgs = agent.db.get_session_messages(&session.id).unwrap();
    let reply1_ts = msgs
        .iter()
        .find(|m| m.role == "assistant" && m.content == "reply1")
        .unwrap()
        .created_at
        .clone();
    let second_id = msgs
        .iter()
        .find(|m| m.content == "second")
        .unwrap()
        .id
        .clone();
    // Snapshot with a branch point ONLY at step 1; the target step 2 has
    // no branch point (realistic: step 2's save_branch_point never ran).
    let canonical = vec![CanonicalMessage {
        role: CanonicalRole::System,
        content: vec![ContentPart::text("sys")],
        tool_calls: None,
        tool_call_id: None,
        reasoning: None,
        web_search_calls: Vec::new(),
        thinking_blocks: Vec::new(),
        source: None,
        id: None,
    }];
    let mut branch_points = HashMap::new();
    branch_points.insert(
        1,
        BranchPoint {
            event_cursor: 1, // CompactSummary seed length
            step_number: 1,
            last_msg_at: Some(reply1_ts),
        },
    );
    let snapshot = ReActSnapshot {
        events: seed_events_from_canonical(canonical),
        step_number: 2,
        branch_points,
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

    // The user clicked "second" (the newest user message, whose id
    // resolves to a persisted row).
    agent
        .rollback_session(&session.id, 2, true, Some(&second_id))
        .await
        .unwrap();
    assert_eq!(
        executor.get_session_state(&session.id).await,
        Some(SessionStatus::Paused)
    );
    let msgs = agent.db.get_session_messages(&session.id).unwrap();
    let contents: Vec<&str> = msgs.iter().map(|m| m.content.as_str()).collect();
    assert_eq!(
        contents,
        vec!["first", "reply1"],
        "rollback must delete the clicked user message and everything after it: {:?}",
        contents
    );
}

#[tokio::test]
async fn rollback_errors_when_target_message_id_does_not_match() {
    // Regression: user-message rollback used to fall back to matching by
    // content when the clicked message's id missed, and to guessing the
    // newest user message when even that failed. Both guesses could
    // delete the wrong message; an unresolvable id is now a direct error
    // and the session is left untouched.
    let (agent, executor) = make_test_agent();
    let session = executor.create_session("strict rollback").await.unwrap();
    agent
        .db
        .add_message(&session.id, "user", "first question", Some("text"), None)
        .unwrap();
    agent
        .db
        .add_message(&session.id, "assistant", "reply A", Some("text"), None)
        .unwrap();
    agent
        .db
        .add_message(&session.id, "user", "second question", Some("text"), None)
        .unwrap();
    agent
        .db
        .add_message(&session.id, "assistant", "reply B", Some("text"), None)
        .unwrap();
    let msgs = agent.db.get_session_messages(&session.id).unwrap();
    let reply_a_ts = msgs
        .iter()
        .find(|m| m.role == "assistant" && m.content == "reply A")
        .unwrap()
        .created_at
        .clone();
    // Branch point at step 1 only; target step 2 has none (fallback).
    let canonical = vec![CanonicalMessage {
        role: CanonicalRole::System,
        content: vec![ContentPart::text("sys")],
        tool_calls: None,
        tool_call_id: None,
        reasoning: None,
        web_search_calls: Vec::new(),
        thinking_blocks: Vec::new(),
        source: None,
        id: None,
    }];
    let mut branch_points = HashMap::new();
    branch_points.insert(
        1,
        BranchPoint {
            event_cursor: 1, // CompactSummary seed length
            step_number: 1,
            last_msg_at: Some(reply_a_ts),
        },
    );
    let snapshot = ReActSnapshot {
        events: seed_events_from_canonical(canonical),
        step_number: 2,
        branch_points,
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

    // The live-view id never matches a DB id and no content fallback
    // exists anymore: rollback must error and delete nothing.
    let err = agent
        .rollback_session(&session.id, 1, true, Some("live-view-local-id"))
        .await
        .unwrap_err();
    assert!(
        err.to_string().contains("not found in session messages"),
        "unexpected error: {}",
        err
    );
    let msgs = agent.db.get_session_messages(&session.id).unwrap();
    assert_eq!(msgs.len(), 4, "no message may be deleted on error");
}

#[tokio::test]
async fn rollback_orphan_after_processed_turn_preserves_earlier_history() {
    let (agent, executor) = make_test_agent();
    let session = executor.create_session("orphan rollback").await.unwrap();
    let msgs = seed_hello_snapshot(&agent, &session.id);

    // Roll back the interrupted message: only it must be discarded; the
    // earlier exchange ("hello" / "thinking") survives.
    let interrupt_id = msgs
        .iter()
        .find(|m| m.content == "interrupt")
        .unwrap()
        .id
        .clone();
    agent
        .rollback_session(&session.id, 1, true, Some(&interrupt_id))
        .await
        .unwrap();
    assert_eq!(
        executor.get_session_state(&session.id).await,
        Some(SessionStatus::Paused)
    );
    let msgs = agent.db.get_session_messages(&session.id).unwrap();
    let contents: Vec<&str> = msgs.iter().map(|m| m.content.as_str()).collect();
    assert_eq!(
        contents,
        vec!["hello", "thinking"],
        "orphan rollback must not wipe earlier history, got {:?}",
        contents
    );
    // The canonical must NOT be truncated: "hello" is a legitimately
    // processed message and stays in the restored context.
    let restored: ReActSnapshot =
        serde_json::from_str(&agent.db.get_react_state(&session.id).unwrap().unwrap()).unwrap();
    let (restored_canonical, _) = restored.project();
    assert!(
        restored_canonical
            .iter()
            .any(|m| m.role == CanonicalRole::User),
        "orphan rollback must not truncate the processed user message from canonical"
    );
}

#[tokio::test]
async fn rollback_processed_user_message_with_later_orphan_wipes_target_timeline() {
    // Same layout as the orphan test, but the user rolls back the
    // PROCESSED message ("hello") rather than the orphan. The orphan's
    // existence must not hijack the rollback: deleting from the target's
    // own timestamp also discards the later orphan (it belongs to the
    // discarded timeline), and the canonical IS truncated.
    let (agent, executor) = make_test_agent();
    let session = executor.create_session("processed rollback").await.unwrap();
    let msgs = seed_hello_snapshot(&agent, &session.id);

    let hello_id = msgs
        .iter()
        .find(|m| m.content == "hello")
        .unwrap()
        .id
        .clone();
    agent
        .rollback_session(&session.id, 1, true, Some(&hello_id))
        .await
        .unwrap();
    assert_eq!(
        executor.get_session_state(&session.id).await,
        Some(SessionStatus::Paused)
    );
    let msgs = agent.db.get_session_messages(&session.id).unwrap();
    assert!(
        msgs.is_empty(),
        "rollback of the processed message must wipe the orphan too, got {:?}",
        msgs.iter().map(|m| &m.content).collect::<Vec<_>>()
    );
    let restored: ReActSnapshot =
        serde_json::from_str(&agent.db.get_react_state(&session.id).unwrap().unwrap()).unwrap();
    let (restored_canonical, _) = restored.project();
    assert!(
        !restored_canonical
            .iter()
            .any(|m| m.role == CanonicalRole::User),
        "rollback of a processed message must truncate the canonical"
    );
}

#[tokio::test]
async fn rollback_pause_uses_target_message_ts_not_latest_user() {
    // A steering interjection persisted between the rolled-back user
    // message and the branch point must NOT hijack the delete range: the
    // target message's own timestamp wins, so the target is removed too.
    let (agent, executor) = make_test_agent();
    let session = executor.create_session("target ts").await.unwrap();
    agent
        .db
        .add_message(&session.id, "user", "hello", Some("text"), None)
        .unwrap();
    agent
        .db
        .add_message(&session.id, "assistant", "thinking", Some("text"), None)
        .unwrap();
    // A steering interjection persisted after "hello" but BEFORE the
    // branch-point thought timestamp (the user typed while the agent was
    // working on the first step).
    agent
        .db
        .add_message(
            &session.id,
            "user",
            "also check the time",
            Some("text"),
            None,
        )
        .unwrap();
    let msgs = agent.db.get_session_messages(&session.id).unwrap();
    let hello_id = msgs
        .iter()
        .find(|m| m.content == "hello")
        .unwrap()
        .id
        .clone();
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
        upgrade_tool_rounds: Vec::new(),
    };
    agent
        .db
        .save_react_state(&session.id, &serde_json::to_string(&snapshot).unwrap())
        .unwrap();

    // Roll back "hello" specifically —the steering interjection must
    // NOT keep "hello" alive.
    agent
        .rollback_session(&session.id, 1, true, Some(&hello_id))
        .await
        .unwrap();
    assert_eq!(
        executor.get_session_state(&session.id).await,
        Some(SessionStatus::Paused)
    );
    let msgs = agent.db.get_session_messages(&session.id).unwrap();
    assert!(
        msgs.is_empty(),
        "rolling back 'hello' must delete it (and the interjection), got {:?}",
        msgs.iter().map(|m| &m.content).collect::<Vec<_>>()
    );
    let restored: ReActSnapshot =
        serde_json::from_str(&agent.db.get_react_state(&session.id).unwrap().unwrap()).unwrap();
    let (restored_canonical, _) = restored.project();
    assert!(
        !restored_canonical
            .iter()
            .any(|m| m.role == CanonicalRole::User),
        "canonical must not keep the rolled-back user message"
    );
}

#[tokio::test]
async fn rollback_pause_matches_prefixed_supplement_in_canonical() {
    // Legacy CompactSummary seeds may still store historically prefixed
    // steering text ("Steering: …") while the DB stores the raw text.
    // Rolling back must match via InjectSource::match_prefixes so the
    // message is removed from the restored context.
    let (agent, executor) = make_test_agent();
    let session = executor.create_session("prefixed rollback").await.unwrap();
    agent
        .db
        .add_message(&session.id, "user", "do it", Some("text"), None)
        .unwrap();
    // The steering is injected BEFORE the step's LLM call, so it is
    // persisted before the branch-point thought timestamp.
    agent
        .db
        .add_message(&session.id, "user", "use French", Some("text"), None)
        .unwrap();
    agent
        .db
        .add_message(&session.id, "assistant", "thinking", Some("text"), None)
        .unwrap();
    let msgs = agent.db.get_session_messages(&session.id).unwrap();
    let steering_id = msgs
        .iter()
        .find(|m| m.content == "use French")
        .unwrap()
        .id
        .clone();
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
            content: vec![ContentPart::text("do it")],
            tool_calls: None,
            tool_call_id: None,
            reasoning: None,
            web_search_calls: Vec::new(),
            thinking_blocks: Vec::new(),
            source: None,
            id: None,
        },
        // The steering was pushed into the canonical with its prefix.
        CanonicalMessage {
            role: CanonicalRole::User,
            content: vec![ContentPart::text("Steering: use French")],
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
        2,
        BranchPoint {
            event_cursor: 1, // CompactSummary seed length
            step_number: 2,
            last_msg_at: Some(thinking_ts),
        },
    );
    let snapshot = ReActSnapshot {
        events: seed_events_from_canonical(canonical),
        step_number: 2,
        branch_points,
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

    agent
        .rollback_session(&session.id, 2, true, Some(&steering_id))
        .await
        .unwrap();
    assert_eq!(
        executor.get_session_state(&session.id).await,
        Some(SessionStatus::Paused)
    );
    let msgs = agent.db.get_session_messages(&session.id).unwrap();
    assert_eq!(
        msgs.len(),
        1,
        "the steering message itself must be deleted, 'do it' stays: {:?}",
        msgs.iter().map(|m| &m.content).collect::<Vec<_>>()
    );
    assert_eq!(msgs[0].content, "do it");
    let restored: ReActSnapshot =
        serde_json::from_str(&agent.db.get_react_state(&session.id).unwrap().unwrap()).unwrap();
    let (restored_canonical, _) = restored.project();
    assert!(
        !restored_canonical
            .iter()
            .any(|m| m.role == CanonicalRole::User
                && m.content.iter().any(|p| matches!(
                    p,
                    ContentPart::Text(t) if t.contains("use French")
                ))),
        "the prefixed steering entry must be trimmed from the canonical"
    );
    assert!(
        restored_canonical
            .iter()
            .any(|m| m.role == CanonicalRole::User
                && m.content
                    .iter()
                    .any(|p| matches!(p, ContentPart::Text(t) if t == "do it"))),
        "'do it' must stay in the canonical"
    );
}

/// R6 W8×O1: rollback while `PausedAwaitingAnswer` must clear the ask gate
/// (memory + snapshot) so the next user input is not mis-routed as an answer.
#[tokio::test]
async fn rollback_while_ask_wait_clears_awaiting_answer_gate() {
    let tools = Arc::new(ToolsManager::new());
    tools
        .registry
        .register(Arc::new(haven_tools::builtin::ask::AskTool) as ToolBox)
        .await;
    let mock = Arc::new(ScriptedMock::new(vec![ScriptedResponse::Chunk(
        StreamChunk {
            text: Some("Need a choice.".into()),
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
        },
    )]));
    let (agent, executor) = make_test_agent_with(mock, tools);
    let collector = Arc::new(EventCollector::new());
    agent.set_emitter(collector);
    let session = executor.create_session("ask then rollback").await.unwrap();
    agent.run_session_from_id(&session.id).await.unwrap();
    assert_eq!(
        executor.get_session_state(&session.id).await,
        Some(SessionStatus::PausedAwaitingAnswer)
    );
    assert!(
        executor.get_awaiting_answer(&session.id).await.is_some(),
        "ask gate must be set before rollback"
    );

    let state_json = agent.db.get_react_state(&session.id).unwrap().unwrap();
    let snap = ReActSnapshot::from_json(&state_json).unwrap();
    assert!(snap.awaiting_answer.is_some());
    let target_step = snap.step_number.max(1);

    agent
        .rollback_session(&session.id, target_step, false, None)
        .await
        .unwrap();

    assert_eq!(
        executor.get_session_state(&session.id).await,
        Some(SessionStatus::Pending)
    );
    assert!(
        executor.get_awaiting_answer(&session.id).await.is_none(),
        "ask gate must be cleared after rollback"
    );
    let restored =
        ReActSnapshot::from_json(&agent.db.get_react_state(&session.id).unwrap().unwrap()).unwrap();
    assert!(
        restored.awaiting_answer.is_none(),
        "snapshot must not resurrect awaiting_answer"
    );
    assert!(restored.awaiting_confirm.is_none());
    assert!(restored.run_budget.is_none());
}

/// R6 W7×O1: rollback during an in-flight tool batch cancels, joins, and
/// restores from the pre-batch branch point without dangling tool_calls.
#[tokio::test]
async fn rollback_mid_tool_batch_joins_and_restores() {
    let tools = Arc::new(ToolsManager::new());
    let timing = Arc::new(TimingState::new());
    tools
        .registry
        .register(Arc::new(TimingTool::new("delay_a", timing.clone())) as ToolBox)
        .await;
    tools
        .registry
        .register(Arc::new(TimingTool::new("delay_b", timing.clone())) as ToolBox)
        .await;
    let mock = Arc::new(ScriptedMock::new(vec![
        ScriptedResponse::Chunk(StreamChunk {
            text: Some("Running both.".into()),
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
            text: Some("Should not run after rollback.".into()),
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
    let session = executor.create_session("rollback mid batch").await.unwrap();

    let run = tokio::spawn({
        let agent = agent.clone();
        let session_id = session.id.clone();
        async move { agent.run_session_from_id(&session_id).await }
    });
    for _ in 0..50 {
        if collector.has_action("delay_a") && collector.has_action("delay_b") {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert!(
        collector.has_action("delay_a") && collector.has_action("delay_b"),
        "batch must have started before rollback"
    );

    agent
        .rollback_session(&session.id, 1, false, None)
        .await
        .unwrap();
    let _ = run.await;

    assert_eq!(
        executor.get_session_state(&session.id).await,
        Some(SessionStatus::Pending)
    );
    assert!(
        !executor.is_run_in_flight(&session.id).await,
        "run slot must be released after rollback join"
    );
    let restored =
        ReActSnapshot::from_json(&agent.db.get_react_state(&session.id).unwrap().unwrap()).unwrap();
    let (canonical, _) = restored.project();
    let dangling = canonical.iter().any(|m| {
        m.role == CanonicalRole::Assistant
            && m.tool_calls.as_ref().is_some_and(|calls| !calls.is_empty())
    });
    assert!(
        !dangling,
        "restored canonical must not end with dangling tool_calls"
    );
}

/// R6: user-edit rollback from ask-wait must reach plain Paused so the
/// next send is not mis-routed as an ask answer (status dual-track gate).
#[tokio::test]
async fn rollback_ask_wait_pause_true_leaves_plain_paused() {
    let tools = Arc::new(ToolsManager::new());
    tools
        .registry
        .register(Arc::new(haven_tools::builtin::ask::AskTool) as ToolBox)
        .await;
    let mock = Arc::new(ScriptedMock::new(vec![ScriptedResponse::Chunk(
        StreamChunk {
            text: Some("Need a choice.".into()),
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
        },
    )]));
    let (agent, executor) = make_test_agent_with(mock, tools);
    let collector = Arc::new(EventCollector::new());
    agent.set_emitter(collector);
    let session = executor
        .create_session("ask then user-edit rollback")
        .await
        .unwrap();
    // create_session does not persist a messages row; user-edit rollback
    // requires an explicit target_message_id.
    let user = agent
        .db
        .add_message(
            &session.id,
            "user",
            "ask then user-edit rollback",
            Some("text"),
            None,
        )
        .unwrap();
    agent.run_session_from_id(&session.id).await.unwrap();
    assert_eq!(
        executor.get_session_state(&session.id).await,
        Some(SessionStatus::PausedAwaitingAnswer)
    );
    let snap =
        ReActSnapshot::from_json(&agent.db.get_react_state(&session.id).unwrap().unwrap()).unwrap();
    let target_step = snap.step_number.max(1);
    agent
        .rollback_session(&session.id, target_step, true, Some(&user.id))
        .await
        .unwrap();
    assert_eq!(
        executor.get_session_state(&session.id).await,
        Some(SessionStatus::Paused),
        "user-edit rollback must leave plain Paused, not PausedAwaitingAnswer"
    );
    assert!(
        !executor.is_ask_gated(&session.id).await,
        "ask gate must be fully clear after user-edit rollback"
    );
}
