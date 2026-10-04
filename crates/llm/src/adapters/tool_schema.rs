use crate::types::sanitize_tool_parameters;
use serde_json::{Map, Value};

/// Project a tool parameter schema into the object-only dialect required by
/// providers whose function-tool parameters must have an object root.
///
/// xAI and OpenAI Responses reject a root-level `anyOf`/`oneOf`/`allOf`, even
/// when the schema also declares `type: object`. The full schema remains
/// authoritative inside Haven: the tool registry validates every model-
/// produced call before execution. This projection widens the model-visible
/// schema by merging branch properties and fields required by every object
/// branch. When a root union has a shared discriminator such as `operation` or
/// `scope`, a compact form of its constraint is retained below
/// `dependentSchemas`; this keeps nested branch unions (for example schedule
/// timing alternatives) visible to the model without repeating every property
/// description under both the root and each dependent branch.
/// Unions without a usable discriminator are widened as before because JSON
/// Schema has no equivalent object-only encoding for an arbitrary root union.
pub(crate) fn project_tool_parameters_for_object_root(schema: Value) -> Value {
    let Value::Object(mut root) = sanitize_tool_parameters(schema) else {
        unreachable!("sanitize_tool_parameters always returns an object");
    };

    let mut branches = Vec::new();
    let mut constraints = Vec::new();
    let mut common_required = None;
    let mut all_required = Vec::new();
    for keyword in ["anyOf", "oneOf", "allOf"] {
        if let Some(Value::Array(items)) = root.remove(keyword) {
            branches.extend(items.iter().cloned());
            let mut constraint = Map::new();
            constraint.insert(keyword.to_string(), Value::Array(items.clone()));
            constraints.push(Value::Object(constraint));

            let branch_required = required_names(&items);
            if keyword == "allOf" {
                all_required.extend(branch_required);
            } else {
                common_required = Some(match common_required {
                    None => branch_required,
                    Some(previous) => intersect_names(previous, branch_required),
                });
            }
        }
    }
    if branches.is_empty() {
        return Value::Object(root);
    }

    let mut properties = match root.remove("properties") {
        Some(Value::Object(properties)) => properties,
        _ => Map::new(),
    };

    for branch in &branches {
        let Value::Object(branch) = branch else {
            continue;
        };

        if let Some(Value::Object(branch_properties)) = branch.get("properties") {
            for (name, schema) in branch_properties {
                match properties.get_mut(name) {
                    Some(existing) if existing != schema => {
                        let merged = merge_xai_property_schemas(existing.clone(), schema.clone());
                        *existing = merged;
                    }
                    Some(_) => {}
                    None => {
                        properties.insert(name.clone(), schema.clone());
                    }
                }
            }
        }
    }

    root.insert("type".into(), Value::String("object".into()));
    root.insert("properties".into(), Value::Object(properties));

    let mut root_required = root
        .remove("required")
        .and_then(|value| value.as_array().cloned())
        .unwrap_or_default();
    for name in common_required.unwrap_or_default() {
        if !root_required
            .iter()
            .any(|value| value.as_str() == Some(&name))
        {
            root_required.push(Value::String(name));
        }
    }
    for name in all_required {
        if !root_required
            .iter()
            .any(|value| value.as_str() == Some(&name))
        {
            root_required.push(Value::String(name));
        }
    }
    if !root_required.is_empty() {
        root.insert("required".into(), Value::Array(root_required));
    }

    if let Some(discriminator) = find_common_discriminator(&branches) {
        let constraint = match constraints.as_slice() {
            [single] => single.clone(),
            [] => unreachable!("root union branches produce a constraint"),
            multiple => serde_json::json!({"allOf": multiple}),
        };
        let mut dependent = match root.remove("dependentSchemas") {
            Some(Value::Object(dependent)) => dependent,
            _ => Map::new(),
        };
        match dependent.remove(&discriminator) {
            Some(existing) if !existing.is_null() => {
                dependent.insert(
                    discriminator.clone(),
                    serde_json::json!({
                        "allOf": [
                            compact_object_root_constraint(existing, &discriminator),
                            compact_object_root_constraint(constraint, &discriminator)
                        ]
                    }),
                );
            }
            _ => {
                dependent.insert(
                    discriminator.clone(),
                    compact_object_root_constraint(constraint, &discriminator),
                );
            }
        }
        root.insert("dependentSchemas".into(), Value::Object(dependent));
    }

    Value::Object(root)
}

/// Keep the discriminator and validation structure in a dependent schema, but
/// remove branch-local property definitions. The flattened root owns those
/// definitions exactly once; required arrays and nested unions remain because
/// they express relationships between fields rather than their descriptions.
fn compact_object_root_constraint(value: Value, discriminator: &str) -> Value {
    let Value::Object(object) = value else {
        return value;
    };

    // A dependent schema may itself be an envelope (`oneOf`/`allOf`) around
    // root branches. Compact only that direct envelope. Once inside a branch,
    // nested unions belong to a property-local object and must keep their own
    // property definitions.
    let envelope_key = ["anyOf", "oneOf", "allOf"]
        .into_iter()
        .find(|key| object.contains_key(*key) && !object.contains_key("properties"));
    if let Some(key) = envelope_key
        && let Some(items) = object.get(key).and_then(Value::as_array).cloned()
    {
        let mut envelope = Map::new();
        for (name, nested) in object {
            if name == key {
                envelope.insert(
                    name,
                    Value::Array(
                        items
                            .iter()
                            .cloned()
                            .map(|item| compact_object_root_branch(item, discriminator))
                            .collect(),
                    ),
                );
            } else if name != "description" && name != "additionalProperties" {
                envelope.insert(name, nested);
            }
        }
        return Value::Object(envelope);
    }

    compact_object_root_branch(Value::Object(object), discriminator)
}

fn compact_object_root_branch(value: Value, discriminator: &str) -> Value {
    let Value::Object(object) = value else {
        return value;
    };

    let mut compact = Map::new();
    for key in [
        "type",
        "required",
        "minProperties",
        "maxProperties",
        "anyOf",
        "oneOf",
        "allOf",
        "not",
        "if",
        "then",
        "else",
    ] {
        let Some(nested) = object.get(key) else {
            continue;
        };
        compact.insert(key.to_string(), nested.clone());
    }

    if let Some(Value::Object(properties)) = object.get("properties")
        && let Some(property) = properties.get(discriminator)
    {
        let mut discriminator_property = Map::new();
        discriminator_property.insert(discriminator.to_string(), property.clone());
        compact.insert("properties".into(), Value::Object(discriminator_property));
    }

    Value::Object(compact)
}

fn required_names(branches: &[Value]) -> Vec<String> {
    let mut common = None;
    for branch in branches {
        let names = branch
            .get("required")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        common = Some(match common {
            None => names,
            Some(previous) => intersect_names(previous, names),
        });
    }
    common.unwrap_or_default()
}

fn intersect_names(left: Vec<String>, right: Vec<String>) -> Vec<String> {
    left.into_iter()
        .filter(|name| right.iter().any(|candidate| candidate == name))
        .collect()
}

fn find_common_discriminator(branches: &[Value]) -> Option<String> {
    let mut common: Option<Vec<String>> = None;
    for branch in branches {
        let mut candidates = Vec::new();
        collect_discriminator_candidates(branch, &mut candidates);
        candidates.sort();
        candidates.dedup();
        common = Some(match common {
            None => candidates,
            Some(previous) => previous
                .into_iter()
                .filter(|name| candidates.iter().any(|candidate| candidate == name))
                .collect(),
        });
    }

    let common = common?;
    ["operation", "scope"]
        .into_iter()
        .find(|preferred| common.iter().any(|candidate| candidate == preferred))
        .map(str::to_owned)
        .or_else(|| common.into_iter().next())
}

fn collect_discriminator_candidates(schema: &Value, candidates: &mut Vec<String>) {
    let Some(object) = schema.as_object() else {
        return;
    };
    if let Some(Value::Object(properties)) = object.get("properties") {
        for (name, property) in properties {
            if property.get("const").is_some()
                || property
                    .get("enum")
                    .and_then(Value::as_array)
                    .is_some_and(|values| !values.is_empty())
            {
                candidates.push(name.clone());
            }
        }
    }
    for keyword in ["anyOf", "oneOf", "allOf"] {
        if let Some(Value::Array(branches)) = object.get(keyword) {
            for branch in branches {
                collect_discriminator_candidates(branch, candidates);
            }
        }
    }
}

/// Project a tool parameter schema into the subset used by Gemini function
/// declarations.
///
/// Gemini documents function parameters as a deliberately small OpenAPI
/// schema subset. The complete schema remains authoritative inside Haven;
/// this projection only shapes what the model sees at the provider boundary.
/// Root and nested object unions are widened into ordinary objects, scalar
/// `const` values become single-value enums, and validation-only keywords that
/// Gemini does not consume are dropped.
pub(crate) fn project_tool_parameters_for_gemini(schema: Value) -> Value {
    project_gemini_schema(sanitize_tool_parameters(schema))
}

fn project_gemini_schema(schema: Value) -> Value {
    let Value::Object(mut map) = schema else {
        return serde_json::json!({});
    };

    let mut branches = Vec::new();
    for keyword in ["anyOf", "oneOf", "allOf"] {
        if let Some(Value::Array(items)) = map.remove(keyword) {
            branches.extend(items);
        }
    }
    if !branches.is_empty() {
        if branches.iter().all(is_object_schema) {
            map.insert("oneOf".into(), Value::Array(branches));
            return project_gemini_schema(project_tool_parameters_for_object_root(Value::Object(
                map,
            )));
        }
        return project_gemini_scalar_union(map, branches);
    }

    let description = map
        .get("description")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let mut projected = Map::new();
    if let Some(description) = description {
        projected.insert("description".into(), Value::String(description));
    }

    let mut nullable = map
        .get("nullable")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let schema_type = match map.get("type") {
        Some(Value::String(schema_type)) => Some(schema_type.clone()),
        Some(Value::Array(types)) => {
            let mut first_type = None;
            for item in types {
                let Some(schema_type) = item.as_str() else {
                    continue;
                };
                if schema_type == "null" {
                    nullable = true;
                } else if first_type.is_none() {
                    first_type = Some(schema_type.to_owned());
                }
            }
            first_type
        }
        _ => None,
    };

    let has_properties = matches!(map.get("properties"), Some(Value::Object(_)));
    let has_items = map.get("items").is_some();
    let schema_type = schema_type.or_else(|| {
        if has_properties {
            Some("object".into())
        } else if has_items {
            Some("array".into())
        } else if map.get("const").is_some() || map.get("enum").is_some() {
            map.get("const")
                .or_else(|| map.get("enum").and_then(|value| value.as_array()?.first()))
                .and_then(gemini_type_of_value)
                .map(str::to_owned)
        } else {
            None
        }
    });

    if let Some(schema_type) = schema_type {
        if matches!(
            schema_type.as_str(),
            "string" | "number" | "integer" | "boolean" | "object" | "array"
        ) {
            projected.insert("type".into(), Value::String(schema_type.clone()));
        }

        if schema_type == "object" {
            let mut properties = Map::new();
            if let Some(Value::Object(input_properties)) = map.get("properties") {
                for (name, property) in input_properties {
                    properties.insert(name.clone(), project_gemini_schema(property.clone()));
                }
            }
            projected.insert("properties".into(), Value::Object(properties));

            if let Some(Value::Array(required)) = map.get("required") {
                let required = required
                    .iter()
                    .filter_map(Value::as_str)
                    .filter(|name| {
                        projected["properties"]
                            .as_object()
                            .is_some_and(|properties| properties.contains_key(*name))
                    })
                    .map(|name| Value::String(name.to_owned()))
                    .collect::<Vec<_>>();
                if !required.is_empty() {
                    projected.insert("required".into(), Value::Array(required));
                }
            }
        } else if schema_type == "array"
            && let Some(items) = map.get("items")
        {
            projected.insert("items".into(), project_gemini_schema(items.clone()));
        }
    }

    let enum_values = if let Some(const_value) = map.get("const") {
        Some(vec![const_value.clone()])
    } else {
        map.get("enum")
            .and_then(Value::as_array)
            .filter(|values| !values.is_empty())
            .cloned()
    };
    if let Some(enum_values) = enum_values {
        projected.insert("enum".into(), Value::Array(enum_values));
    }
    if nullable {
        projected.insert("nullable".into(), Value::Bool(true));
    }
    if let Some(format) = map.get("format").and_then(Value::as_str) {
        projected.insert("format".into(), Value::String(format.to_owned()));
    }

    Value::Object(projected)
}

fn project_gemini_scalar_union(mut metadata: Map<String, Value>, branches: Vec<Value>) -> Value {
    let mut projected = Map::new();
    if let Some(description) = metadata.remove("description")
        && description.is_string()
    {
        projected.insert("description".into(), description);
    }

    let mut nullable = metadata
        .remove("nullable")
        .and_then(|value| value.as_bool())
        .unwrap_or(false);
    let mut enum_values = Vec::new();
    let mut branch_types = Vec::new();
    for branch in branches {
        let Value::Object(branch) = branch else {
            continue;
        };
        if branch.get("type").and_then(Value::as_str) == Some("null") {
            nullable = true;
        } else if let Some(schema_type) = branch
            .get("type")
            .and_then(Value::as_str)
            .or_else(|| branch.get("const").and_then(gemini_type_of_value))
        {
            branch_types.push(schema_type.to_owned());
        }
        if let Some(const_value) = branch.get("const") {
            if !enum_values.iter().any(|value| value == const_value) {
                enum_values.push(const_value.clone());
            }
        } else if let Some(values) = branch.get("enum").and_then(Value::as_array) {
            for value in values {
                if !enum_values.iter().any(|existing| existing == value) {
                    enum_values.push(value.clone());
                }
            }
        }
    }

    let schema_type = branch_types.first().cloned();
    if let Some(schema_type) = schema_type
        && matches!(
            schema_type.as_str(),
            "string" | "number" | "integer" | "boolean" | "object" | "array"
        )
    {
        projected.insert("type".into(), Value::String(schema_type));
    }
    if !enum_values.is_empty() {
        projected.insert("enum".into(), Value::Array(enum_values));
    }
    if nullable {
        projected.insert("nullable".into(), Value::Bool(true));
    }
    Value::Object(projected)
}

fn is_object_schema(schema: &Value) -> bool {
    schema.get("type").and_then(Value::as_str) == Some("object")
        || schema.get("properties").is_some_and(Value::is_object)
}

fn gemini_type_of_value(value: &Value) -> Option<&'static str> {
    match value {
        Value::String(_) => Some("string"),
        Value::Number(_) => Some("number"),
        Value::Bool(_) => Some("boolean"),
        Value::Array(_) => Some("array"),
        Value::Object(_) => Some("object"),
        Value::Null => None,
    }
}

fn merge_xai_property_schemas(left: Value, right: Value) -> Value {
    let mut values = Vec::new();
    for schema in [&left, &right] {
        if let Some(value) = schema.get("const") {
            if !values.iter().any(|existing| existing == value) {
                values.push(value.clone());
            }
        } else if let Some(enum_values) = schema.get("enum").and_then(Value::as_array) {
            for value in enum_values {
                if !values.iter().any(|existing| existing == value) {
                    values.push(value.clone());
                }
            }
        }
    }
    if !values.is_empty() {
        let mut merged = Map::new();
        let left_type = left.get("type").and_then(Value::as_str);
        let right_type = right.get("type").and_then(Value::as_str);
        let property_type = match (left_type, right_type) {
            (Some(left_type), Some(right_type)) if left_type == right_type => Some(left_type),
            (Some(left_type), None) | (None, Some(left_type)) => Some(left_type),
            _ => None,
        };
        if let Some(property_type) = property_type {
            merged.insert("type".into(), Value::String(property_type.into()));
        }
        merged.insert("enum".into(), Value::Array(values));
        return Value::Object(merged);
    }

    serde_json::json!({"anyOf": [left, right]})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn object_root_tool_parameter_projection_flattens_root_unions() {
        let projected = project_tool_parameters_for_object_root(serde_json::json!({
            "type": "object",
            "properties": {
                "operation": { "type": "string", "enum": ["set", "list"] }
            },
            "oneOf": [
                {
                    "type": "object",
                    "properties": {
                        "operation": { "const": "set" },
                        "body": { "type": "string" }
                    },
                    "required": ["operation", "body"]
                },
                {
                    "type": "object",
                    "properties": { "operation": { "const": "list" } },
                    "required": ["operation"]
                }
            ]
        }));

        assert_eq!(projected["type"], "object");
        assert!(projected.get("oneOf").is_none());
        assert_eq!(
            projected["properties"]["operation"]["enum"],
            serde_json::json!(["set", "list"])
        );
        assert!(projected["properties"]["body"].is_object());
        assert_eq!(projected["required"], serde_json::json!(["operation"]));
        assert_eq!(
            projected["dependentSchemas"]["operation"]["oneOf"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
        assert!(
            projected["dependentSchemas"]["operation"]["oneOf"][0]["properties"]["body"].is_null()
        );
        assert!(
            projected["dependentSchemas"]["operation"]["oneOf"][0]
                .get("additionalProperties")
                .is_none()
        );
    }

    #[test]
    fn object_root_tool_parameter_projection_sanitizes_non_object_roots() {
        for schema in [
            Value::Null,
            Value::String("invalid".into()),
            Value::Bool(true),
        ] {
            let projected = project_tool_parameters_for_object_root(schema);
            assert_eq!(projected["type"], "object");
            assert!(projected["properties"].is_object());
        }
    }

    #[test]
    fn object_root_tool_parameter_projection_removes_every_root_union_keyword() {
        let projected = project_tool_parameters_for_object_root(serde_json::json!({
            "anyOf": [
                {
                    "type": "object",
                    "properties": { "operation": { "const": "read" } },
                    "required": ["operation"]
                },
                {
                    "type": "object",
                    "properties": { "operation": { "const": "write" } },
                    "required": ["operation"]
                }
            ],
            "allOf": [
                {
                    "type": "object",
                    "properties": {
                        "operation": { "const": "read" },
                        "trace": { "type": "boolean" }
                    },
                    "required": ["operation", "trace"]
                }
            ]
        }));

        assert_eq!(projected["type"], "object");
        for keyword in ["anyOf", "oneOf", "allOf"] {
            assert!(projected.get(keyword).is_none(), "root contains {keyword}");
        }
        assert!(projected["dependentSchemas"]["operation"]["allOf"][0]["anyOf"].is_array());
        assert!(projected["dependentSchemas"]["operation"]["allOf"][1]["allOf"].is_array());
    }

    #[test]
    fn object_root_tool_parameter_projection_adds_object_root_and_merges_const_values() {
        let projected = project_tool_parameters_for_object_root(serde_json::json!({
            "oneOf": [
                {
                    "type": "object",
                    "properties": { "operation": { "const": "get" } },
                    "required": ["operation"]
                },
                {
                    "type": "object",
                    "properties": { "operation": { "const": "set" } },
                    "required": ["operation", "value"]
                }
            ]
        }));

        assert_eq!(projected["type"], "object");
        assert_eq!(
            projected["properties"]["operation"]["enum"],
            serde_json::json!(["get", "set"])
        );
        assert_eq!(projected["required"], serde_json::json!(["operation"]));
    }

    #[test]
    fn object_root_tool_parameter_projection_preserves_nested_unions() {
        let projected = project_tool_parameters_for_object_root(serde_json::json!({
            "type": "object",
            "properties": {
                "value": {
                    "anyOf": [{ "type": "string" }, { "type": "integer" }]
                }
            },
            "oneOf": [
                { "type": "object", "properties": { "value": {} } },
                { "type": "object", "properties": { "value": {} } }
            ]
        }));

        assert!(projected.get("oneOf").is_none());
        assert!(projected["properties"]["value"].get("anyOf").is_some());
    }

    #[test]
    fn object_root_tool_parameter_projection_keeps_discriminated_nested_union_constraints() {
        let projected = project_tool_parameters_for_object_root(serde_json::json!({
            "type": "object",
            "properties": {
                "operation": { "type": "string", "enum": ["set", "list"] },
                "delay_secs": { "type": "integer" },
                "due_at": { "type": "string" }
            },
            "oneOf": [
                {
                    "type": "object",
                    "properties": { "operation": { "const": "list" } },
                    "required": ["operation"]
                },
                {
                    "type": "object",
                    "properties": {
                        "operation": { "const": "set" },
                        "delay_secs": { "type": "integer" },
                        "due_at": { "type": "string" }
                    },
                    "required": ["operation"],
                    "oneOf": [
                        {
                            "properties": { "delay_secs": { "type": "integer", "minimum": 1 } },
                            "required": ["delay_secs"]
                        },
                        { "required": ["due_at"] }
                    ]
                }
            ]
        }));

        assert!(projected.get("oneOf").is_none());
        let branches = projected["dependentSchemas"]["operation"]["oneOf"]
            .as_array()
            .unwrap();
        assert_eq!(branches.len(), 2);
        assert_eq!(
            branches[1]["oneOf"][0]["required"],
            serde_json::json!(["delay_secs"])
        );
        assert!(branches[1]["oneOf"][0]["properties"]["delay_secs"].is_object());
    }

    #[test]
    fn gemini_tool_parameter_projection_uses_openapi_subset() {
        let projected = project_tool_parameters_for_gemini(serde_json::json!({
            "type": "object",
            "properties": {
                "operation": { "type": "string", "enum": ["set", "list"] },
                "delay_secs": { "type": "integer", "minimum": 1 },
                "body": { "type": "string", "minLength": 1 },
                "metadata": {
                    "type": "object",
                    "properties": {
                        "mode": { "const": "safe", "description": "execution mode" }
                    },
                    "required": ["mode"]
                }
            },
            "oneOf": [
                {
                    "type": "object",
                    "properties": { "operation": { "const": "set" } },
                    "required": ["operation", "body"]
                },
                {
                    "type": "object",
                    "properties": { "operation": { "const": "list" } },
                    "required": ["operation"]
                }
            ]
        }));

        assert_eq!(projected["type"], "object");
        assert!(projected.get("oneOf").is_none());
        assert_eq!(
            projected["properties"]["operation"]["enum"],
            serde_json::json!(["set", "list"])
        );
        assert!(
            projected["properties"]["delay_secs"]
                .get("minimum")
                .is_none()
        );
        assert!(projected["properties"]["body"].get("minLength").is_none());
        assert_eq!(
            projected["properties"]["metadata"]["properties"]["mode"]["enum"],
            serde_json::json!(["safe"])
        );
    }

    #[test]
    fn gemini_tool_parameter_projection_handles_scalar_union() {
        let projected = project_tool_parameters_for_gemini(serde_json::json!({
            "type": "object",
            "properties": {
                "value": {
                    "anyOf": [
                        { "type": "string" },
                        { "type": "null" }
                    ]
                }
            }
        }));

        assert_eq!(projected["properties"]["value"]["type"], "string");
        assert_eq!(projected["properties"]["value"]["nullable"], true);
        assert!(projected["properties"]["value"].get("anyOf").is_none());
    }
}
