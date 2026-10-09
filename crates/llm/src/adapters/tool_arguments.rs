//! Provider wire argument conversion for canonical tool calls.
//!
//! `CanonicalToolCall` stores parsed JSON shared with the Agent. Wire strings
//! and incomplete-stream repair belong to the adapter boundary that reads and
//! writes provider payloads.

use serde_json::Value;

/// Serialize canonical tool arguments for a provider request.
pub(crate) fn serialize_tool_arguments(arguments: &Value) -> String {
    serde_json::to_string(arguments).expect("JSON values must be serializable")
}

/// Parse tool arguments after the provider has completed the response.
///
/// Empty / whitespace-only input means a tool with no arguments and becomes
/// `{}`. Valid JSON is preserved. Structural truncation (missing `}` / `]` or
/// a value after `:`) is repaired only after completion; a mid-string cut or
/// unrepairable value becomes `Null`, allowing the ReAct loop to retry.
pub(crate) fn parse_completed_tool_arguments(args: &str) -> Value {
    match parse_provider_tool_arguments(args) {
        ToolArgumentsParse::Empty => serde_json::json!({}),
        ToolArgumentsParse::Valid(value) | ToolArgumentsParse::Repaired(value) => value,
        ToolArgumentsParse::Incomplete => Value::Null,
    }
}

/// Result of parsing provider tool-call argument JSON.
#[derive(Debug, Clone, PartialEq)]
enum ToolArgumentsParse {
    /// No arguments text yet (or whitespace-only).
    Empty,
    /// Parsed without repair.
    Valid(Value),
    /// Parsed only after structural truncation repair (missing closers / value).
    Repaired(Value),
    /// Mid-string cut or still invalid after repair.
    Incomplete,
}

fn parse_provider_tool_arguments(args: &str) -> ToolArgumentsParse {
    let trimmed = args.trim();
    if trimmed.is_empty() {
        return ToolArgumentsParse::Empty;
    }
    if let Ok(value) = serde_json::from_str(trimmed) {
        return ToolArgumentsParse::Valid(value);
    }
    match repair_truncated_json(trimmed) {
        Some(RepairOutcome {
            value,
            closed_open_string: false,
        }) => ToolArgumentsParse::Repaired(value),
        _ => ToolArgumentsParse::Incomplete,
    }
}

struct RepairOutcome {
    value: Value,
    /// True when the input ended inside a JSON string — the closed value is
    /// a guess and must not be treated as complete arguments.
    closed_open_string: bool,
}

/// Best-effort repair for tool-call argument JSON cut off mid-stream
/// (cancel, idle timeout, or `finish_reason=length` while arguments were
/// still being generated). Closes an open string, completes a bare object
/// key with `:null`, fills a missing value with `null`, strips a trailing
/// comma, and closes unmatched `{` / `[`.
fn repair_truncated_json(input: &str) -> Option<RepairOutcome> {
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Expect {
        ObjectKey,
        ObjectColon,
        ObjectValue,
        ArrayValue,
        CommaOrClose,
    }

    let mut out = String::with_capacity(input.len() + 16);
    let mut stack: Vec<char> = Vec::new();
    let mut expect = Expect::ObjectValue;
    let mut in_string = false;
    let mut escape = false;
    let mut closed_open_string = false;

    for ch in input.chars() {
        if in_string {
            out.push(ch);
            if escape {
                escape = false;
            } else if ch == '\\' {
                escape = true;
            } else if ch == '"' {
                in_string = false;
                expect = if expect == Expect::ObjectKey {
                    Expect::ObjectColon
                } else {
                    Expect::CommaOrClose
                };
            }
            continue;
        }
        if ch.is_whitespace() {
            out.push(ch);
            continue;
        }
        match ch {
            '"' => {
                in_string = true;
                out.push(ch);
            }
            '{' => {
                stack.push('{');
                expect = Expect::ObjectKey;
                out.push(ch);
            }
            '[' => {
                stack.push('[');
                expect = Expect::ArrayValue;
                out.push(ch);
            }
            '}' | ']' => {
                let open = if ch == '}' { '{' } else { '[' };
                if stack.last() != Some(&open) {
                    return None;
                }
                stack.pop();
                out.push(ch);
                expect = Expect::CommaOrClose;
            }
            ':' => {
                expect = Expect::ObjectValue;
                out.push(ch);
            }
            ',' => {
                expect = if stack.last() == Some(&'[') {
                    Expect::ArrayValue
                } else {
                    Expect::ObjectKey
                };
                out.push(ch);
            }
            _ => {
                out.push(ch);
                expect = Expect::CommaOrClose;
            }
        }
    }

    if in_string {
        closed_open_string = true;
        if escape {
            out.pop();
        }
        out.push('"');
        expect = if expect == Expect::ObjectKey {
            Expect::ObjectColon
        } else {
            Expect::CommaOrClose
        };
    }

    // Strip trailing comma BEFORE filling missing values — otherwise
    // `["a",` becomes `["a",null]`.
    {
        let trimmed_len = out.trim_end().len();
        out.truncate(trimmed_len);
        if out.ends_with(',') {
            out.pop();
            expect = Expect::CommaOrClose;
        }
    }

    match expect {
        Expect::ObjectColon => out.push_str(":null"),
        Expect::ObjectValue | Expect::ArrayValue => out.push_str("null"),
        Expect::ObjectKey | Expect::CommaOrClose => {}
    }

    while let Some(open) = stack.pop() {
        out.push(if open == '{' { '}' } else { ']' });
    }

    let value = serde_json::from_str(&out).ok()?;
    Some(RepairOutcome {
        value,
        closed_open_string,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serialization_preserves_canonical_json() {
        assert_eq!(
            serialize_tool_arguments(&serde_json::json!({"path": "/tmp/a"})),
            r#"{"path":"/tmp/a"}"#
        );
    }

    #[test]
    fn completed_empty_arguments_become_object() {
        assert_eq!(parse_completed_tool_arguments(""), serde_json::json!({}));
        assert_eq!(parse_completed_tool_arguments("  "), serde_json::json!({}));
        assert_eq!(parse_provider_tool_arguments(""), ToolArgumentsParse::Empty);
    }

    #[test]
    fn completed_arguments_repair_structural_truncation_only() {
        // Mid-string cut: value is unknown → Null / incomplete (retry).
        assert_eq!(
            parse_completed_tool_arguments(r#"{"path":"/tmp/fo"#),
            Value::Null
        );
        assert_eq!(
            parse_provider_tool_arguments(r#"{"path":"/tmp/fo"#),
            ToolArgumentsParse::Incomplete
        );
        assert_eq!(parse_completed_tool_arguments(r#"{"a":1,"b"#), Value::Null);

        // Structural only: missing closers / missing value after `:`.
        let v3 = parse_completed_tool_arguments(r#"{"a":"#);
        assert!(v3["a"].is_null());
        assert!(matches!(
            parse_provider_tool_arguments(r#"{"a":"#),
            ToolArgumentsParse::Repaired(_)
        ));

        let v4 = parse_completed_tool_arguments(r#"{"items":[1,2"#);
        assert_eq!(v4["items"], serde_json::json!([1, 2]));

        // Trailing comma must not invent a null element.
        let v4c = parse_completed_tool_arguments(r#"{"items":[1,2,"#);
        assert_eq!(v4c["items"], serde_json::json!([1, 2]));
        assert!(matches!(
            parse_provider_tool_arguments(r#"{"items":[1,2,"#),
            ToolArgumentsParse::Repaired(_)
        ));

        let v5 = parse_completed_tool_arguments(r#"{"path":"/tmp/foo","mode":"r""#);
        assert_eq!(v5["path"], "/tmp/foo");
        assert_eq!(v5["mode"], "r");
    }

    #[test]
    fn unrepairable_completed_arguments_stay_null() {
        // Truncated literal cannot be closed into valid JSON.
        assert_eq!(parse_completed_tool_arguments(r#"{"ok":tru"#), Value::Null);
        assert_eq!(
            parse_provider_tool_arguments(r#"{"ok":tru"#),
            ToolArgumentsParse::Incomplete
        );
    }
}
