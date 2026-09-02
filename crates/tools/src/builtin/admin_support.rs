use serde_json::Value;

/// Recursively remove credentials from model-visible configuration projections.
pub(crate) fn mask_sensitive_config(value: &mut Value) {
    match value {
        Value::Object(map) => {
            for (key, value) in map.iter_mut() {
                let key_lower = key.to_ascii_lowercase();
                if (key_lower.ends_with("api_key")
                    || key_lower.ends_with("api_secret")
                    || key_lower.ends_with("password")
                    || key_lower.ends_with("access_token"))
                    && value.as_str().is_some_and(|secret| !secret.is_empty())
                {
                    *value = Value::String("[masked]".into());
                } else if key_lower == "env" {
                    if let Value::Array(entries) = value {
                        for entry in entries {
                            if let Some(entry_text) = entry.as_str() {
                                let name = entry_text
                                    .split_once('=')
                                    .map(|(name, _)| name.trim())
                                    .filter(|name| !name.is_empty())
                                    .unwrap_or("value");
                                *entry = Value::String(format!("{name}=[masked]"));
                            } else {
                                *entry = Value::String("[masked]".into());
                            }
                        }
                    }
                } else {
                    mask_sensitive_config(value);
                }
            }
        }
        Value::Array(values) => {
            for value in values.iter_mut() {
                mask_sensitive_config(value);
            }
        }
        _ => {}
    }
}

/// Resolve a dotted path inside a JSON tree, descending through object keys
/// and numeric array indices.
pub(crate) fn value_at<'a>(root: &'a Value, path: &str) -> Option<&'a Value> {
    let mut current = root;
    for segment in path.split('.') {
        match (current, segment.parse::<usize>()) {
            (Value::Array(values), Ok(index)) => current = values.get(index)?,
            (Value::Object(map), _) => current = map.get(segment)?,
            _ => return None,
        }
    }
    Some(current)
}
