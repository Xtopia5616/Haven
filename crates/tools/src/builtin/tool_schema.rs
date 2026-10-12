use serde_json::{Value, json};

/// Build a JSON Schema string for Haven's canonical entity ID format.
///
/// Runtime validation remains owned by `haven_common::types::is_canonical_id`.
/// Exact length bounds close the final-newline behavior of regex `$` anchors.
pub(super) fn canonical_entity_id_schema(prefix: &str, description: Option<&str>) -> Value {
    let id_length = prefix.len() + 1 + 32;
    let mut schema = json!({
        "type": "string",
        "minLength": id_length,
        "maxLength": id_length,
        "pattern": format!("^{prefix}-[0-9a-f]{{32}}$")
    });
    if let Some(description) = description {
        schema["description"] = json!(description);
    }
    schema
}

#[cfg(test)]
mod tests {
    use super::canonical_entity_id_schema;
    use haven_common::types::is_canonical_id;
    use serde_json::json;

    #[test]
    fn canonical_id_schemas_match_the_common_runtime_validator() {
        for prefix in ["asset", "ses", "msg", "toolrun"] {
            let schema = canonical_entity_id_schema(prefix, None);
            let validator = jsonschema::validator_for(&schema).expect("valid id schema");
            let valid_id = format!("{prefix}-{}", "a".repeat(32));
            let candidates = [
                valid_id.clone(),
                format!("{prefix}-{}", "a".repeat(31)),
                format!("{prefix}-{}", "A".repeat(32)),
                format!("{valid_id}\n"),
                format!("other-{}", "a".repeat(32)),
            ];

            for candidate in candidates {
                assert_eq!(
                    validator.is_valid(&json!(candidate)),
                    is_canonical_id(&candidate, prefix),
                    "schema mismatch for prefix {prefix} and value {candidate:?}"
                );
            }
        }
    }

    #[test]
    fn canonical_id_schema_preserves_its_description() {
        let schema = canonical_entity_id_schema("asset", Some("managed asset"));

        assert_eq!(schema["description"], "managed asset");
        assert_eq!(schema["minLength"], 38);
        assert_eq!(schema["maxLength"], 38);
    }
}
