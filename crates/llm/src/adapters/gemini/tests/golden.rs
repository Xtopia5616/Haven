use super::super::*;

#[test]
fn response_fixture_maps_to_canonical() {
    let payload: GeminiResponse =
        serde_json::from_str(include_str!("fixtures/response.json")).unwrap();
    let response = GeminiAdapter::new(ModelEndpoint::default())
        .parse_response(payload, Some("gemini-test".into()))
        .unwrap();
    assert_eq!(response.text, "Hello");
    assert_eq!(response.finish_reason, Some(FinishReason::Stop));
    assert_eq!(response.usage.total_tokens, 2);
    assert_eq!(response.model.as_deref(), Some("gemini-test"));
}

#[test]
fn request_fixture_pins_generate_content_wire_keys() {
    let expected: Value = serde_json::from_str(include_str!("fixtures/request.json")).unwrap();
    let endpoint = ModelEndpoint {
        model_name: "gemini-test".into(),
        max_tokens: 128,
        ..Default::default()
    };
    let actual = serde_json::to_value(GeminiAdapter::new(endpoint).build_request_body(
        Vec::new(),
        Vec::new(),
        false,
    ))
    .unwrap();
    for key in [
        "temperature",
        "seed",
        "top_p",
        "top_k",
        "frequency_penalty",
        "presence_penalty",
        "topP",
        "topK",
    ] {
        let config = actual.get("generationConfig").unwrap_or(&actual);
        assert!(config.get(key).is_none(), "removed sampling key {key}");
    }
    assert_eq!(actual["generationConfig"], expected["generationConfig"]);
}

#[test]
fn stream_fixture_reuses_generate_content_wire_shape() {
    let payload: GeminiResponse =
        serde_json::from_str(include_str!("fixtures/stream.jsonl")).unwrap();
    assert_eq!(payload.candidates.unwrap().len(), 1);
}
