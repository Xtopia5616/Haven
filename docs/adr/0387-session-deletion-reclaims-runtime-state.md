# ADR 0387: Reclaim per-session runtime state on deletion

- Status: accepted (2026-09-28)
- Scope: ordered `session:deleted` delivery and process-local session caches/workers
- Related: [ADR 0349](0349-session-terminal-cleanup-audit.md), [ADR 0376](0376-session-ui-obsolete-compatibility-removal.md)

## Context

Agent events are queued in `BufferedEmitter`, while deletion notifications used to bypass that queue through a Tauri command. The UI could therefore process deletion and then receive an older streamed event that recreated transcript state. Deletion also left `PartialStore`, Windows notification, MemoryWorker, and `UsageRuntime` state associated with the removed session.

## Decision

1. Represent deletion as an `AgentEvent::SessionDeleted` and publish it through the installed Agent event bus after the session actor has quiesced. The Tauri adapter keeps the existing `session:deleted` payload and channel. The buffered emitter preserves the tombstone under overflow, including when its queue contains only stream-reset markers.
2. Remove the deleted session's partial checkpoint row and process-local generation/content/lock entries only after its checkpoint writer has joined. History clear does the same for all sessions after every actor has quiesced.
3. Have `AgentLayer` own delete and clear orchestration. After durable deletion it clears MemoryWorker's dirty/throttle/prefetch state, clears ReAct context caches, and removes each UsageRuntime entry. Usage workers receive a FIFO shutdown operation, close their receiver, and are joined before their tracker and epoch state are released.
4. Clear the matching Windows notification title/status cache when the ordered deletion event reaches `TauriEmitter`; a global clear empties both maps.

## Alternatives considered

- Awaiting a generic queue drain and then emitting directly from the command was rejected because it would keep deletion transport split across the app command and Agent event path.
- Relying only on front-end frame cancellation was rejected because backend queue ordering can deliver an event after the frame has already been cleared.
- Retaining UsageRuntime workers until application shutdown was rejected now that session deletion provides a quiescence boundary and worker join protocol.

## Impact and verification

The public event name and payload are unchanged, and no database schema or durable event contract changes. Deletion now waits for accepted UsageRuntime operations and worker shutdown; a failed queued operation is reported to its caller as stopped. Regression coverage checks buffered event order and overflow preservation, partial checkpoint cleanup, notification-cache reclamation, and UsageRuntime worker/state reclamation. No data reset is required.

Rollback reverts the event routing and lifecycle cleanup changes. Deleted session rows continue to use the existing deletion and foreign-key cleanup behavior.
