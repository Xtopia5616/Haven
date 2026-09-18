# ADR-0167: Reuse ReAct Turn Preparation Results

- Status: Accepted
- Date: 2026-09-18
- Owners: Haven maintainers

## Context

The production `before_step` hook resolves the session tool surface so
compaction can include the exact schema budget. `run_turn` then resolved the
same surface again before starting the model request. The tool-definition
cache avoided rebuilding schemas, but each turn still paid for another
catalog-version lookup and cache traversal.

## Decision

`before_step` returns immutable turn-preparation values. Production hooks pass
the already resolved `Arc<Vec<ToolDefinition>>` to `run_turn`; no-op/test hooks
return an empty preparation and retain the existing fallback build path.

The hook remains responsible for preparation and compaction ordering, while
the turn remains responsible for provider request construction and execution.
Tool catalog versioning and cache invalidation remain authoritative in
`ToolsManager` and `ReActEngine::build_tool_definitions_for_session`.

## Consequences

- The normal ReAct path removes one per-step catalog-version lookup.
- Tool schema identity and mid-run loading semantics do not change.
- Custom/test hooks do not need to implement tool preparation.
- A future preparation value can carry other immutable preflight results
  without adding another side channel to the loop.

## Verification and rollback

Verified with `cargo check --workspace --locked`,
`cargo clippy --workspace --locked -- -D warnings`, and
`cargo test --locked -p haven-agent`.

Rollback is limited to removing the returned preparation value and restoring
the turn-local tool-definition lookup; no persisted data or wire contract is
affected.
