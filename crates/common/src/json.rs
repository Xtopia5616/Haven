use serde_json::Value;

/// Recursively sort JSON object keys while preserving array order.
///
/// Object key order does not change the represented JSON data, while array
/// order can carry schema or request meaning and therefore remains intact.
pub fn canonicalize_json(value: Value) -> Value {
    match value {
        Value::Array(values) => Value::Array(values.into_iter().map(canonicalize_json).collect()),
        Value::Object(object) => {
            let mut entries: Vec<_> = object.into_iter().collect();
            entries.sort_by(|left, right| left.0.cmp(&right.0));
            Value::Object(
                entries
                    .into_iter()
                    .map(|(key, value)| (key, canonicalize_json(value)))
                    .collect(),
            )
        }
        other => other,
    }
}

/// Serialize JSON using [`canonicalize_json`] ordering.
pub fn canonical_json_bytes(value: &Value) -> Vec<u8> {
    serde_json::to_vec(&canonicalize_json(value.clone())).unwrap_or_default()
}
