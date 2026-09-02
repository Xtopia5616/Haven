use super::support::*;
use super::*;

#[test]
fn sanitize_canonical_drops_orphaned_tool_messages_and_dangling_calls() {
    // Mirrors the corruption found in a real interrupted session: a
    // compaction split the assistant(tool_calls)/tool-results pair, so
    // the summary assistant (no tool_calls) is followed by orphaned tool
    // messages. A valid pair and a dangling trailing assistant follow —
    // the dangling call is repaired with an Interrupted result instead of
    // being dropped.
    let mut canonical = vec![
        make_canonical(CanonicalRole::System, "sys"),
        make_canonical(CanonicalRole::User, "hello"),
        make_canonical(CanonicalRole::Assistant, "[Compacted summary]"),
        make_tool_result("call_00_a", "result a"),
        make_tool_result("call_01_b", "result b"),
        make_assistant_with_calls(&["call_00_c", "call_01_d"]),
        make_tool_result("call_00_c", "result c"),
        make_tool_result("call_01_d", "result d"),
        make_assistant_with_calls(&["call_00_e"]),
    ];
    let repairs = sanitize_canonical(&mut canonical);
    assert_eq!(repairs, 1, "one dangling trailing call must be repaired");

    let roles: Vec<CanonicalRole> = canonical.iter().map(|m| m.role).collect();
    assert_eq!(
        roles,
        vec![
            CanonicalRole::System,
            CanonicalRole::User,
            CanonicalRole::Assistant,
            CanonicalRole::Assistant,
            CanonicalRole::Tool,
            CanonicalRole::Tool,
            CanonicalRole::Assistant,
            CanonicalRole::Tool,
        ],
        "orphaned tools must be dropped and the dangling tool_call repaired with an Interrupted result"
    );
    // The surviving pair's tool results are intact.
    assert_eq!(canonical[4].tool_call_id.as_deref(), Some("call_00_c"));
    assert_eq!(canonical[5].tool_call_id.as_deref(), Some("call_01_d"));
    // The dangling trailing call was answered with an Interrupted result.
    assert_eq!(canonical[7].tool_call_id.as_deref(), Some("call_00_e"));
    assert!(canonical[7].content.iter().any(|p| matches!(
        p,
        ContentPart::Text(t) if t.contains("Interrupted")
    )));
}

#[test]
fn sanitize_canonical_repairs_partial_tool_batch() {
    // The real interrupted-batch failure: an assistant declared TWO tool
    // calls but only one result came back (the other tool was cut off
    // mid-execution). Providers reject an incomplete batch with a 400, so
    // the missing result must be repaired with an Interrupted one.
    let mut canonical = vec![
        make_canonical(CanonicalRole::User, "hi"),
        make_assistant_with_calls(&["call_a", "call_b"]),
        make_tool_result("call_a", "result a"),
    ];
    let repairs = sanitize_canonical(&mut canonical);
    assert_eq!(repairs, 1, "missing call_b result must count as one repair");

    let roles: Vec<CanonicalRole> = canonical.iter().map(|m| m.role).collect();
    assert_eq!(
        roles,
        vec![
            CanonicalRole::User,
            CanonicalRole::Assistant,
            CanonicalRole::Tool,
            CanonicalRole::Tool,
        ],
        "the missing tool_call result must be repaired with an Interrupted result"
    );
    assert_eq!(canonical[2].tool_call_id.as_deref(), Some("call_a"));
    assert_eq!(canonical[3].tool_call_id.as_deref(), Some("call_b"));
    assert!(canonical[3].content.iter().any(|p| matches!(
        p,
        ContentPart::Text(t)
            if t.contains("Interrupted") && t.contains("tool:") && t.contains("arguments")
    )));
}

#[test]
fn sanitize_canonical_repairs_interrupted_call_before_user_message() {
    // A dangling tool_call followed by a new user message: the interrupted
    // call must get an Interrupted result inserted before the user message
    // (it can no longer be trimmed as trailing), keeping the array valid.
    let mut canonical = vec![
        make_canonical(CanonicalRole::User, "a"),
        make_assistant_with_calls(&["call_1"]),
        make_canonical(CanonicalRole::User, "next"),
    ];
    let repairs = sanitize_canonical(&mut canonical);
    assert_eq!(
        repairs, 1,
        "dangling call before user must count as one repair"
    );

    let roles: Vec<CanonicalRole> = canonical.iter().map(|m| m.role).collect();
    assert_eq!(
        roles,
        vec![
            CanonicalRole::User,
            CanonicalRole::Assistant,
            CanonicalRole::Tool,
            CanonicalRole::User,
        ],
        "an Interrupted result must be inserted between the dangling call and the next user message"
    );
    assert_eq!(canonical[2].tool_call_id.as_deref(), Some("call_1"));
}

#[test]
fn sanitize_canonical_keeps_user_reset_and_trailing_tool() {
    // A tool message following a user message is orphaned; a trailing
    // tool message after its assistant-with-calls is valid.
    let mut canonical = vec![
        make_canonical(CanonicalRole::User, "a"),
        make_assistant_with_calls(&["call_1"]),
        make_tool_result("call_1", "r"),
        make_canonical(CanonicalRole::User, "b"),
        make_tool_result("call_1", "orphan after user"),
    ];
    let repairs = sanitize_canonical(&mut canonical);
    assert_eq!(
        repairs, 0,
        "dropping an orphaned tool is not a repair insert"
    );

    let roles: Vec<CanonicalRole> = canonical.iter().map(|m| m.role).collect();
    assert_eq!(
        roles,
        vec![
            CanonicalRole::User,
            CanonicalRole::Assistant,
            CanonicalRole::Tool,
            CanonicalRole::User,
        ],
        "only the orphaned trailing tool must be removed"
    );
}

#[test]
fn sanitize_canonical_healthy_path_returns_zero_repairs() {
    // Phase 7 / J2: a well-formed tool chain must be a no-op (repair
    // count 0). This is the counter-test counterpart to the warn metric
    // on the LLM gate; debug builds do not assert in the hot path because
    // interrupt/cancel recovery legitimately repairs.
    let mut canonical = vec![
        make_canonical(CanonicalRole::User, "hi"),
        make_assistant_with_calls(&["call_a"]),
        make_tool_result("call_a", "ok"),
        make_canonical(CanonicalRole::Assistant, "done"),
    ];
    let before_len = canonical.len();
    let repairs = sanitize_canonical(&mut canonical);
    assert_eq!(repairs, 0);
    assert_eq!(canonical.len(), before_len);
    debug_assert_eq!(repairs, 0);
}
