# ADR 0289: Agent Session Metadata Reads Through SessionStore

- Status: Accepted
- Date: 2026-09-24
- Scope: Agent action-completion status lookup and peer-session inspection
- Related: [ADR 0249](0249-session-store-session-record-reads.md), [ADR 0277](0277-context-source-session-title-port.md), [ADR 0288](0288-agent-title-generation-session-store-port.md)

## Context

Two `AgentLayer` reads still used its raw `Database` field after the executor
missed a session: action-completion delivery loaded only the persisted status,
and peer inspection loaded the persisted session record. `SessionSupervisor`
already exposes its shared `SessionStore`, whose asynchronous
`load_session_record` port runs the existing query on the blocking pool.

## Decision

1. Action-completion status lookup continues to prefer the executor status.
   After an executor miss, it loads the record through
   `SessionSupervisor::session_store().load_session_record()` and maps the
   record status. Store errors and missing records both remain `None`, matching
   the existing best-effort fallback.
2. Peer inspection continues to prefer the executor session. After an executor
   miss, it loads the full record through the same typed port. Store errors
   continue to propagate, and a missing record keeps the exact
   `session '<id>' not found` error. The result still maps the stored ID,
   status string, terminal flag, title, and `timed_out = false`.
3. Peer wait polling, action completion delivery and wake behavior, and all
   other `AgentLayer` database paths remain unchanged. No Store API, DTO,
   schema, IPC, or persistence contract is added.

Using the existing record port avoids adding a status-only duplicate API or a
new Agent persistence abstraction. Keeping the two existing fallbacks separate
preserves their distinct error semantics.

## Impact and Verification

- Removes exactly two production session reads from `AgentLayer.db`.
- An Agent unit test covers store fallback status and peer record mapping,
  not-found behavior, and executor-first precedence with an in-memory database.
- Validate with formatting, `haven-agent` tests and strict Clippy; run workspace
  check, strict Clippy, and tests when resources permit.

## Rollback

Restore the two `Database::run_blocking` reads in `layer.rs` and remove this ADR,
its index and roadmap entries. No schema or user data reset is required.
