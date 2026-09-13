use serde_json::{Map, Value};

/// Expand nested oneOf schemas into leaf object branches while preserving
/// parent properties and required fields. Grouped tools use this to compose
/// already-reviewed operation schemas without creating a second validation
/// contract.
pub(crate) fn expand_branches(schema: &Value) -> Vec<Value> {
    let Some(branches) = schema.get("oneOf").and_then(Value::as_array) else {
        return vec![schema.clone()];
    };

    let mut parent = schema.clone();
    if let Some(object) = parent.as_object_mut() {
        object.remove("oneOf");
    }

    branches
        .iter()
        .flat_map(expand_branches)
        .map(|branch| merge_branch(&parent, &branch))
        .collect()
}

fn merge_branch(parent: &Value, child: &Value) -> Value {
    let (Some(parent), Some(child)) = (parent.as_object(), child.as_object()) else {
        return child.clone();
    };

    let mut merged = parent.clone();
    if child.get("additionalProperties") == Some(&Value::Bool(false)) {
        // A strict leaf owns its complete property allowlist. The broad
        // top-level properties commonly used for documentation must not be
        // copied into it or cross-operation arguments would become valid.
        merged.remove("properties");
    }
    for (key, value) in child {
        if key == "properties" {
            let mut properties = merged
                .remove("properties")
                .and_then(|value| value.as_object().cloned())
                .unwrap_or_default();
            if let Some(child_properties) = value.as_object() {
                properties.extend(child_properties.clone());
            }
            merged.insert("properties".into(), Value::Object(properties));
        } else if key == "required" {
            let mut required = merged
                .remove("required")
                .and_then(|value| value.as_array().cloned())
                .unwrap_or_default();
            if let Some(child_required) = value.as_array() {
                for item in child_required {
                    if !required.contains(item) {
                        required.push(item.clone());
                    }
                }
            }
            merged.insert("required".into(), Value::Array(required));
        } else {
            merged.insert(key.clone(), value.clone());
        }
    }
    Value::Object(merged)
}

/// Rewrite operation const values in every leaf branch and return the public
/// operation names. Branches without a discriminator are omitted; callers can
/// add explicit branches for schemas that need a separate discriminator.
pub(crate) fn rename_operation_branches(schema: &Value, prefix: &str) -> Vec<(String, Value)> {
    expand_branches(schema)
        .into_iter()
        .filter_map(|mut branch| {
            let operation = branch
                .get("properties")
                .and_then(|properties| properties.get("operation"))
                .and_then(|operation| operation.get("const"))
                .and_then(Value::as_str)?;
            let public_operation = format!("{prefix}{operation}");
            if let Some(operation_schema) = branch
                .get_mut("properties")
                .and_then(Value::as_object_mut)
                .and_then(|properties| properties.get_mut("operation"))
            {
                operation_schema["const"] = Value::String(public_operation.clone());
            }
            Some((public_operation, branch))
        })
        .collect()
}

pub(crate) fn grouped_schema(operation_names: &[String], branches: Vec<Value>) -> Value {
    let mut properties = Map::new();
    properties.insert(
        "operation".into(),
        serde_json::json!({ "type": "string", "enum": operation_names }),
    );
    serde_json::json!({
        "type": "object",
        "properties": properties,
        "required": ["operation"],
        "oneOf": branches,
    })
}

pub(crate) fn operation_branch(operation: &str, properties: Value, required: &[&str]) -> Value {
    let mut branch = serde_json::json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "operation": { "const": operation },
        },
        "required": required,
    });
    if let Some(branch_properties) = branch["properties"].as_object_mut()
        && let Some(properties) = properties.as_object()
    {
        branch_properties.extend(properties.clone());
    }
    branch
}
