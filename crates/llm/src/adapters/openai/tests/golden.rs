use super::super::*;

#[test]
fn response_fixture_maps_to_canonical() {
    let payload: OpenAiResponse =
        serde_json::from_str(include_str!("fixtures/chat-response.json")).unwrap();
    let response = OpenAiAdapter::new(ModelEndpoint::default())
        .parse_openai_response(
            payload,
            Some("gpt-test".into()),
            CacheDiagnostics::default(),
        )
        .unwrap();
    assert_eq!(response.text, "Hello");
    assert_eq!(response.finish_reason, Some(FinishReason::Stop));
    assert_eq!(response.usage.total_tokens, 2);
    assert_eq!(response.model.as_deref(), Some("gpt-test"));
}

#[test]
fn request_fixture_pins_openai_wire_keys() {
    let expected: Value = serde_json::from_str(include_str!("fixtures/chat-request.json")).unwrap();
    let endpoint = ModelEndpoint {
        model_name: "gpt-test".into(),
        max_tokens: 128,
        temperature: 0.2,
        ..Default::default()
    };
    let actual = serde_json::to_value(OpenAiAdapter::new(endpoint).build_request_body(
        Vec::new(),
        Vec::new(),
        false,
    ))
    .unwrap();
    for key in ["model", "max_tokens", "stream", "temperature"] {
        assert_eq!(actual[key], expected[key], "wire key {key}");
    }
}

#[test]
fn stream_fixture_keeps_chat_delta_wire_shape() {
    let payload: OpenAiStreamResponse =
        serde_json::from_str(include_str!("fixtures/chat-stream.jsonl")).unwrap();
    assert_eq!(payload.choices.len(), 1);
    assert_eq!(
        payload.choices[0]
            .delta
            .as_ref()
            .and_then(|message| message.content.as_deref()),
        Some("Hello")
    );
}
