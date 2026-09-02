use super::support::*;
use super::*;

#[tokio::test]
async fn invalid_tool_inputs_are_reported_without_repairing_arguments() {
    let tools = Arc::new(ToolsManager::new());
    tools
        .registry
        .register(Arc::new(ActionRequiredTool) as ToolBox)
        .await;
    let client = Arc::new(FinalAnswerMock) as Arc<dyn LlmClient>;
    let (agent, executor) = make_test_agent_with(client, tools);
    let session = executor.create_session("do it").await.unwrap();

    // Missing required fields is reported; validation never invents
    // side-effecting values.
    let mut actions = vec![Action {
        tool_name: "action_required".into(),
        tool_input: serde_json::json!({}),
        is_final: false,
        tool_call_id: Some("call_1".into()),
    }];
    let repaired = agent
        .react_engine
        .validate_tool_inputs(&session.id, &mut actions)
        .await;
    assert_eq!(repaired.len(), 1);
    let input = &actions[0].tool_input;
    assert_eq!(input, &serde_json::json!({}));
    assert!(repaired[0].render().contains("MISSING REQUIRED FIELD"));
}

#[tokio::test]
async fn valid_tool_inputs_and_final_actions_have_no_validation_failures() {
    let tools = Arc::new(ToolsManager::new());
    tools
        .registry
        .register(Arc::new(ActionRequiredTool) as ToolBox)
        .await;
    let client = Arc::new(FinalAnswerMock) as Arc<dyn LlmClient>;
    let (agent, executor) = make_test_agent_with(client, tools);
    let session = executor.create_session("do it").await.unwrap();

    // Complete call: no validation failure.
    let mut actions = vec![Action {
        tool_name: "action_required".into(),
        tool_input: serde_json::json!({"action": "stop", "query": "hi"}),
        is_final: false,
        tool_call_id: Some("call_1".into()),
    }];
    let repaired = agent
        .react_engine
        .validate_tool_inputs(&session.id, &mut actions)
        .await;
    assert_eq!(repaired.len(), 0);

    // Final actions are not validated in the tool batch.
    let mut actions = vec![Action {
        tool_name: "action_required".into(),
        tool_input: serde_json::json!({}),
        is_final: true,
        tool_call_id: None,
    }];
    let repaired = agent
        .react_engine
        .validate_tool_inputs(&session.id, &mut actions)
        .await;
    assert_eq!(repaired.len(), 0);
}

#[tokio::test]
async fn null_tool_input_is_reported_without_repairing_arguments() {
    // Interrupted/truncated generation yields unparseable arguments,
    // which parse_default_model_response converts to Null. Report the
    // malformed input instead of shipping a guessed object to the tool.
    let tools = Arc::new(ToolsManager::new());
    tools
        .registry
        .register(Arc::new(ActionRequiredTool) as ToolBox)
        .await;
    let client = Arc::new(FinalAnswerMock) as Arc<dyn LlmClient>;
    let (agent, executor) = make_test_agent_with(client, tools);
    let session = executor.create_session("do it").await.unwrap();

    let mut actions = vec![Action {
        tool_name: "action_required".into(),
        tool_input: serde_json::Value::Null,
        is_final: false,
        tool_call_id: Some("call_1".into()),
    }];
    let repaired = agent
        .react_engine
        .validate_tool_inputs(&session.id, &mut actions)
        .await;
    assert_eq!(repaired.len(), 1);
    assert_eq!(actions[0].tool_input, serde_json::Value::Null);
    assert!(repaired[0].render().contains("validation failed"));
}

#[tokio::test]
async fn null_valued_tool_fields_are_reported_without_repairing_arguments() {
    // A required field explicitly set to null is as unusable as a
    // missing one: the validator rejects null for typed fields.
    let tools = Arc::new(ToolsManager::new());
    tools
        .registry
        .register(Arc::new(ActionRequiredTool) as ToolBox)
        .await;
    let client = Arc::new(FinalAnswerMock) as Arc<dyn LlmClient>;
    let (agent, executor) = make_test_agent_with(client, tools);
    let session = executor.create_session("do it").await.unwrap();

    let mut actions = vec![Action {
        tool_name: "action_required".into(),
        tool_input: serde_json::json!({"action": null, "query": null}),
        is_final: false,
        tool_call_id: Some("call_1".into()),
    }];
    let repaired = agent
        .react_engine
        .validate_tool_inputs(&session.id, &mut actions)
        .await;
    assert_eq!(repaired.len(), 1);
    assert_eq!(
        actions[0].tool_input,
        serde_json::json!({"action": null, "query": null})
    );
}

#[tokio::test]
async fn missing_enum_field_is_reported_without_guessing_a_value() {
    let tools = Arc::new(ToolsManager::new());
    tools
        .registry
        .register(Arc::new(EnumRequiredTool) as ToolBox)
        .await;
    let client = Arc::new(FinalAnswerMock) as Arc<dyn LlmClient>;
    let (agent, executor) = make_test_agent_with(client, tools);
    let session = executor.create_session("do it").await.unwrap();

    let mut actions = vec![Action {
        tool_name: "enum_required".into(),
        tool_input: serde_json::json!({}),
        is_final: false,
        tool_call_id: Some("call_1".into()),
    }];
    let repaired = agent
        .react_engine
        .validate_tool_inputs(&session.id, &mut actions)
        .await;
    assert_eq!(repaired.len(), 1);
    assert_eq!(actions[0].tool_input, serde_json::json!({}));
    assert!(repaired[0].render().contains("operation"));
}

#[tokio::test]
async fn invalid_enum_value_is_reported_without_repairing_arguments() {
    // The `action` field is PRESENT but its value is not in the schema
    // enum. Strict providers validate tool_use input against the declared
    // schema and reject the request with a 400 ("Failed to deserialize
    // the JSON body into the target type: input.action: ...") — the value
    // must be reported before execution, not replaced with a guessed
    // discriminator.
    let tools = Arc::new(ToolsManager::new());
    tools
        .registry
        .register(Arc::new(ActionRequiredTool) as ToolBox)
        .await;
    let client = Arc::new(FinalAnswerMock) as Arc<dyn LlmClient>;
    let (agent, executor) = make_test_agent_with(client, tools);
    let session = executor.create_session("do it").await.unwrap();

    let mut actions = vec![Action {
        tool_name: "action_required".into(),
        tool_input: serde_json::json!({"action": "bogus", "query": "hi"}),
        is_final: false,
        tool_call_id: Some("call_1".into()),
    }];
    let repaired = agent
        .react_engine
        .validate_tool_inputs(&session.id, &mut actions)
        .await;
    assert_eq!(repaired.len(), 1);
    assert_eq!(
        actions[0].tool_input,
        serde_json::json!({"action": "bogus", "query": "hi"})
    );
}

#[tokio::test]
async fn wrong_type_tool_value_is_reported_without_repairing_arguments() {
    // Same provider 400 when a field's value type contradicts the schema
    // (e.g. a number where the schema declares a string enum).
    let tools = Arc::new(ToolsManager::new());
    tools
        .registry
        .register(Arc::new(ActionRequiredTool) as ToolBox)
        .await;
    let client = Arc::new(FinalAnswerMock) as Arc<dyn LlmClient>;
    let (agent, executor) = make_test_agent_with(client, tools);
    let session = executor.create_session("do it").await.unwrap();

    let mut actions = vec![Action {
        tool_name: "action_required".into(),
        tool_input: serde_json::json!({"action": 42, "query": "hi"}),
        is_final: false,
        tool_call_id: Some("call_1".into()),
    }];
    let repaired = agent
        .react_engine
        .validate_tool_inputs(&session.id, &mut actions)
        .await;
    assert_eq!(repaired.len(), 1);
    assert_eq!(
        actions[0].tool_input,
        serde_json::json!({"action": 42, "query": "hi"})
    );
}

#[tokio::test]
async fn valid_enum_values_have_no_validation_failures() {
    // A value that conforms to the schema (in the enum, correct type)
    // must NOT be repaired.
    let tools = Arc::new(ToolsManager::new());
    tools
        .registry
        .register(Arc::new(ActionRequiredTool) as ToolBox)
        .await;
    let client = Arc::new(FinalAnswerMock) as Arc<dyn LlmClient>;
    let (agent, executor) = make_test_agent_with(client, tools);
    let session = executor.create_session("do it").await.unwrap();

    let mut actions = vec![Action {
        tool_name: "action_required".into(),
        tool_input: serde_json::json!({"action": "stop", "query": "hi"}),
        is_final: false,
        tool_call_id: Some("call_1".into()),
    }];
    let repaired = agent
        .react_engine
        .validate_tool_inputs(&session.id, &mut actions)
        .await;
    assert_eq!(repaired.len(), 0);
    assert_eq!(actions[0].tool_input["action"], "stop");
}

#[tokio::test]
async fn invalid_optional_field_is_reported_without_repairing_arguments() {
    // Even a non-required property with an invalid value can trip the
    // provider's deserialization (the input object is validated as a
    // whole), so it is reported too.
    let tools = Arc::new(ToolsManager::new());
    tools
        .registry
        .register(Arc::new(EnumWithOptionalTool) as ToolBox)
        .await;
    let client = Arc::new(FinalAnswerMock) as Arc<dyn LlmClient>;
    let (agent, executor) = make_test_agent_with(client, tools);
    let session = executor.create_session("do it").await.unwrap();

    let mut actions = vec![Action {
        tool_name: "enum_with_optional".into(),
        tool_input: serde_json::json!({"operation": "type", "optional": "nope"}),
        is_final: false,
        tool_call_id: Some("call_1".into()),
    }];
    let repaired = agent
        .react_engine
        .validate_tool_inputs(&session.id, &mut actions)
        .await;
    assert_eq!(repaired.len(), 1);
    assert_eq!(actions[0].tool_input["operation"], "type");
    assert_eq!(actions[0].tool_input["optional"], "nope");
}

#[tokio::test]
async fn confirmation_recovery_matches_the_full_invocation_identity() {
    let tools = Arc::new(ToolsManager::new());
    let client = Arc::new(FinalAnswerMock) as Arc<dyn LlmClient>;
    let (_agent, executor) = make_test_agent_with(client, tools);
    let session = executor.create_session("confirm").await.unwrap();
    let pending = ConfirmPending {
        step_number: 3,
        tools: vec![
            ConfirmPendingTool {
                confirm_id: "conf-a".into(),
                tool_name: "run_command".into(),
                tool_input: serde_json::json!({"command":"same"}),
                tool_call_id: "call-a".into(),
                step_id: "step-3".into(),
                action_index: 0,
                risk_level: RiskLevel::High,
                decision: Some(true),
            },
            ConfirmPendingTool {
                confirm_id: "conf-b".into(),
                tool_name: "run_command".into(),
                tool_input: serde_json::json!({"command":"same"}),
                tool_call_id: "call-b".into(),
                step_id: "step-3".into(),
                action_index: 1,
                risk_level: RiskLevel::High,
                decision: Some(false),
            },
        ],
    };
    let restored: ConfirmPending =
        serde_json::from_str(&serde_json::to_string(&pending).unwrap()).unwrap();
    executor
        .set_awaiting_confirm(&session.id, Some(restored))
        .await;

    assert_eq!(
        executor
            .confirm_decision_for(&session.id, "step-3", 0, Some("call-a"))
            .await,
        Some(true)
    );
    assert_eq!(
        executor
            .confirm_decision_for(&session.id, "step-3", 1, Some("call-b"))
            .await,
        Some(false)
    );
    assert_eq!(
        executor
            .confirm_decision_for(&session.id, "step-3", 0, Some("call-b"))
            .await,
        None,
        "same tool name and args must not associate the wrong call"
    );
}
