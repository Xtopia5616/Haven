# ADR 0386: Shared occurrence identity for session terminal fan-out

- Status: accepted (2026-09-28)
- Scope: `SessionCompleted` / `SessionError` Tauri payloads, paired `session:updated` projections, and chat terminal cleanup
- Related: [ADR 0349](0349-session-terminal-cleanup-audit.md), [ADR 0347](0347-agent-event-contract-validation-boundary.md)

## Context

ADR 0349 found that one terminal occurrence is emitted on a primary channel and then projected to `session:updated`. The chat route listened to both and ran terminal cleanup twice. The repeated cleanup dispatches could allocate new reducer maps and notify selectors even where no visible state changed. The lifecycle projection still has independent consumers and also represents standalone terminal changes, so matching by session, status, payload, adjacency, or Tauri event envelope is unsafe.

## Decision

1. Mint one ephemeral `occ-{uuid32}` `occurrence_id` for each `SessionCompleted` / `SessionError` fan-out. Put the same ID on the primary payload and the corresponding `session:updated` payload. The startup session-error forwarder follows the same rule.
2. Keep the field optional and omit it from standalone lifecycle updates. Keep channels, payload ordering, notification ownership, status/title/error/reason projections, and independent lifecycle behavior.
3. The chat handler records explicit terminal occurrence IDs from either channel. The first paired projection handled owns cleanup; its counterpart still runs its reducer projection but skips cleanup and refresh. A standalone update without an ID performs the full cleanup. The per-controller set is bounded to 64 recent IDs so failed or delayed event delivery cannot grow it without limit.
4. `session/status-updated` and inactive `session/termination-shown` return the existing reducer state when the projected values are unchanged. This prevents equivalent lifecycle projections from allocating state and notifying selectors; it is state equality, not event inference.
5. Keep the layout's secondary status handling because it clears busy-session state; keep primary-only terminal notifications and the coalesced MemoryView refresh.

## Alternatives considered

- Inferring a pair from session ID/status/payload or arrival order is rejected: standalone terminal `session:updated` events share those values.
- Moving all cleanup to the secondary channel would make failed secondary delivery lose cleanup currently provided by the primary path.
- Removing either channel would break its distinct toast, busy-state, memory refresh, or lifecycle projection owner.

## Impact and verification

The IPC DTO gains an optional field on `session:completed`, `session:error`, and paired terminal `session:updated` payloads. The frontend mapper validates and converts it to `occurrenceId`. No database, durable event, session storage, resume, or schema changes are made. Existing consumers may ignore the additive field.

Regression coverage checks that Rust primary/secondary payload builders share an ID, the frontend mapper preserves it, paired chat cleanup and reducer notifications run once, identical reducer projections are no-ops, and standalone terminal `session:updated` still cleans up live messages. UI check/test/build and IPC event/contract scripts verify the frontend boundary; Rust app-binary tests verify the wire projection.

Rollback reverts the optional field, occurrence tracking, reducer equality guards, and tests/docs. No data reset is needed.
