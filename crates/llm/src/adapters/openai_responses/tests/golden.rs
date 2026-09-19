use super::super::*;

#[test]
fn response_fixture_maps_to_canonical() {
    let payload: ResponsesResponse =
        serde_json::from_str(include_str!("fixtures/response.json")).unwrap();
    let response = OpenAiResponsesAdapter::new(ModelEndpoint::default())
        .parse_response(payload, Some("gpt-test".into()))
        .unwrap();
    assert_eq!(response.text, "Hello");
    assert_eq!(response.finish_reason, Some(FinishReason::Stop));
    assert_eq!(response.usage.total_tokens, 2);
    assert_eq!(response.model.as_deref(), Some("gpt-test"));
}

#[test]
fn request_fixture_pins_responses_wire_keys() {
    let expected: Value = serde_json::from_str(include_str!("fixtures/request.json")).unwrap();
    let endpoint = ModelEndpoint {
        model_name: "gpt-test".into(),
        max_tokens: 128,
        temperature: 0.2,
        ..Default::default()
    };
    let actual = serde_json::to_value(OpenAiResponsesAdapter::new(endpoint).build_request_body(
        Vec::new(),
        Vec::new(),
        false,
    ))
    .unwrap();
    for key in ["model", "max_output_tokens", "stream", "temperature"] {
        assert_eq!(actual[key], expected[key], "wire key {key}");
    }
}

#[test]
fn stream_fixture_preserves_responses_event_tag() {
    let event: ResponsesStreamEvent =
        serde_json::from_str(include_str!("fixtures/stream.jsonl")).unwrap();
    assert!(matches!(
        event,
        ResponsesStreamEvent::OutputTextDelta { delta: Some(text) } if text == "Hello"
    ));
}
