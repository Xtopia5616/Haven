# ADR 0344: Action lifecycle UI projection boundary

- Status: Implemented
- Date: 2026-09-25
- Scope: Frontend board refresh and `action:finished` projection for background and scheduled actions
- Related: [ADR 0335](0335-action-board-ui-contract-mapper.md), [ADR 0338](0338-action-tail-output-policy-and-snapshot.md), [ADR 0343](0343-action-trigger-policy-boundary.md)

## Context

Phase 7/8 asked whether background and scheduled actions duplicate kind/status
normalization, running/terminal membership, output-tail projection, terminal
projection, or event deduplication.

The audit found a shared action DTO mapper and an authoritative id-keyed
`actionStore` (mirrored into the layout's Svelte `activities` value). The mirror
is derived state, not a second lifecycle reducer. Both `list_actions` rows and lifecycle events pass through
`contracts/action.ts::mapActionPayload`; known status fallback, unknown-kind
rejection, and field validation are already defined there (ADR 0335). Board
capacity uses one `waiting`/`running` predicate. Created, updated, and output
events all upsert partial payloads. Refresh request ordinals and
`actionStateVersion` prevent stale command responses from replacing newer
state; they are hydration race guards, not event deduplication.

The terminal paths remain intentionally different:

- Background `action:finished` upserts the final payload so bound tool cards can
  observe it, projects the result into the owning session, and conditionally
  notifies when that session is not active. A later board refresh excludes
  terminal background history from the live registry.
- Scheduled `action:finished` removes the row from the pending board. The Agent
  notification event provides the user notification. Scheduled actions do not
  publish `action:output` or own a tail.
- `TaskCenter` uses kind-specific status text, timing, and cancellation affordances.

The layout handler duplicated the already-tested
`finalizeBackgroundActionMessages` helper by rebuilding the result content and
dispatching `session/background-result` inline. There is no common terminal
reducer that preserves both completion paths. The Tauri envelope `id` is not a
durable action event identity, and the frontend has no lifecycle-event dedup
mechanism.

## Decision

- Keep the existing id-keyed `actionStore`, mapper, shared live-row predicate,
  and generic create/update/output upserts.
- Reuse `finalizeBackgroundActionMessages(payload)` from the background
  `action:finished` handler instead of duplicating its result projection.
- Keep background finish upsert/result/conditional toast and scheduled finish
  removal/Agent notification as distinct paths. Do not add a cross-kind Job
  state machine or event deduplication.
- Keep the existing Rust wire DTO, channels, ActionService lifecycle,
  event identity/order, tail ownership, and notification behavior unchanged.

## Consequences

The same tested helper now owns the background terminal transcript projection
for the `actionStore` API and the global listener. Regression tests cover live
refresh membership for both kinds, filtering terminal background history, and
the rule that scheduled completion does not rewrite a background tool card.
No Rust DTO, IPC, persisted data, or ActionService behavior changes.

Remaining boundaries are explicit: scheduled terminal history is loaded by its
history command, while background terminal payload briefly reaches bound tool
cards; scheduled has no live tail; and repeated lifecycle notifications are
not deduplicated in UI. Any future deduplication needs a stable event identity
and producer/replay contract.

## Verification

- `cd ui; corepack pnpm run check`
- `cd ui; corepack pnpm run test:run`
- `cd ui; corepack pnpm run build`
- `git diff --cached --check`

## Rollback

Restore the inline background terminal projection in `+layout.svelte`, remove
the added UI regression cases, and revert this ADR plus its architecture and
roadmap entries. No schema, wire, configuration, or user-data reset is needed.
