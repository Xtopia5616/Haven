# ADR 0171: Concurrent Session Dispatch and Lifecycle Admission

- Status: Accepted
- Date: 2026-09-19
- Owners: Haven maintainers

## Context

Haven has one actor as the owner of each session runtime, but several paths
can reach that runtime concurrently: the FIFO dispatcher, direct resume or
continue calls, optimistic UI submissions, session deletion, and history
cleanup. The old boundaries allowed three classes of races:

- resizing a Tokio semaphore while runs were active could leave the effective
  capacity different from the configured limit;
- a dispatcher claim, direct resume, or lifecycle cleanup could observe a
  partially changed actor registry and durable session row;
- the UI used one process-wide submission queue, so sending to session B could
  wait behind an unrelated request in session A.

Cleanup also used to continue after a run-exit timeout. That allowed a late
  tool result or actor command to target state that had already been removed.

## Decision

- Keep `SessionActor` as the single owner of mutable per-session runtime state.
  The supervisor owns one dispatcher and rejects duplicate dispatcher starts.
- Replace the dynamically resized semaphore with `RunAdmission`, which tracks
  `limit` and `active` under one mutex. Every dispatcher run and direct run
  owns exactly one RAII permit for its whole run. Lowering the limit affects
  only future admissions and cannot manufacture stale capacity when old runs
  release.
- Serialize actor-registry and durable-session lifecycle operations with a
  supervisor lifecycle gate. Creation, loading, deletion, and history purge
  use the same gate; deletion first records a session-scoped closing marker so
  direct runs and dispatcher claims cannot reopen the session between quiesce
  and mutation. Direct runs re-check actor membership under the gate after
  waiting for admission, and delete/clear cancels direct admission waiters.
- Make destructive cleanup two-phase: first block new lifecycle/dispatch
  admissions and cancel/dequeue/join every current run, then mutate the actor
  registry and durable rows. A run-exit timeout is an error and aborts the
  destructive mutation instead of proceeding fail-open.
- Keep session message submission lanes in the UI keyed by the captured target
  session. Each lane is FIFO and merges only identical in-flight submissions;
  different session lanes run concurrently. The draft lane remains singular
  until its first created session is adopted, preventing duplicate session
  creation and queued-message overtaking.

## Alternatives considered

- A resizable Tokio semaphore was rejected because reducing permits while all
  permits are held cannot express an exact active-run limit without stale
  capacity accounting.
- Holding the lifecycle mutex while waiting for a run slot was rejected: an
  active run may need the same gate to finish child-session or persistence
  work, creating a deadlock. Admission waits happen before the short registry
  re-check.
- Deleting the database row first or clearing only the in-memory map was
  rejected because either order leaves a window for stale actor resurrection
  or late writes.
- A single global UI submission queue was rejected because it serializes
  independent conversations and makes session switching appear hung.

## Consequences

The effective concurrency limit is deterministic during live reconfiguration;
one session cannot have two active runs, and independent sessions can use
available capacity in parallel. Delete and clear operations now have an
explicit quiescence boundary, and timeout failures remain visible to callers.
The UI can still preserve strict ordering where it matters while avoiding
cross-session head-of-line blocking.

The lifecycle gate and admission state are process-local. Durable
`session_events`, messages, and session rows keep their existing schema and
authority; no IPC or database migration is introduced.

## Verification and rollback/reset

Regression coverage exercises admission resize after active permits, duplicate
dispatcher protection, actor/row deletion together, run-exit timeout
propagation, cancelling a direct resume waiting for capacity, same-session
FIFO, independent-session parallel submission, and draft-lane ordering. Run
the workspace Rust tests and the UI check/test gates before release.

Rollback is a source revert. If a release exposes a new lifecycle failure,
stop dispatch and reset the affected test-version session state according to
the existing release/reset procedure; no schema rollback is required.
