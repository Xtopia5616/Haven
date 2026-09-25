# ADR 0335: Action board UI contract mapper

- Status: Accepted
- Date: 2026-09-25
- Scope: `ActionEvent` rows returned by `list_actions` and the four action lifecycle events
- Related: [ADR 0215](0215-action-board-typed-projection.md), [ADR 0275](0275-action-service-typed-agent-projections.md), [ADR 0330](0330-session-lifecycle-ui-contract-mapper.md)

## Context

The app-binary already owns a named `ActionEvent` wire DTO in `events.rs`. It is
returned by `list_actions` and emitted on `action:created`, `action:updated`,
`action:output`, and `action:finished`. The UI had a duplicate snake_case
`ActionWirePayload` interface and a mapper that trusted its compile-time type;
the event listener also used a cast, so malformed native values could reach
the action store and global layout.

The ActionService completion outbox is a separate internal agent contract. Its
`BackgroundActionCompletion`/scheduled completion values carry lifecycle and
execution data that are not the ActionEvent board projection.

## Decision

- Treat `crates/app-binary/src/events.rs::ActionEvent` and its `ActionKind` as
  the wire authority. Do not change Rust DTOs, Tauri command/event names, wire
  fields, or registration points.
- Make `ui/src/lib/contracts/action.ts::mapActionPayload(unknown)` the single
  runtime validator and snake_case-to-camelCase mapper. Both
  `actionStore.refreshActions` command rows and `events.ts` action listeners
  call it; delete the duplicate TypeScript wire interface and unchecked casts.
- Require a non-empty string `id` and a known `kind`. A malformed required
  field or declared optional field with an invalid type drops the complete row
  or event. Unknown additive fields are ignored. Preserve the existing
  unknown-status downgrade to `failed`; an unknown `kind` fails closed because
  it selects different board, cancellation, and completion behavior and has no
  safe fallback.
- A dropped event/row may produce a generic warning that names only the
  channel or row category. It must not include the payload or user-controlled
  values.
- Keep dynamic `tool_args` as JSON at its existing ActionService execution and
  completion boundary. It is deliberately absent from the renderer-safe
  `ActionEvent`; this mapper ignores it rather than stringifying or forwarding
  it. `ActionCompletion`/outbox ownership and peer inspect behavior remain
  untouched.

## Consequences

The board hydration command and lifecycle events now share a runtime-checked
wire boundary and one camelCase UI DTO. Action fields, ordering, pagination,
running/terminal filtering, reducer updates, event ordering, idempotency,
completion delivery, notification text, and UI behavior remain unchanged.
Unknown status strings keep the prior failed-state fallback; unknown kinds and
malformed rows/events are discarded before consumers see them. No codegen or
second event registration is added.

Remaining handwritten UI contract mirrors include command request/response
contracts, recording events, settings contracts, session-internal camelCase
types/field mappings, and the agent/app event domains. Their migration and any
future generation approach remain separate slices.

## Verification

- Mapper tests cover ordinary and optional fields, unknown status/kind,
  malformed required fields, and opaque JSON `tool_args` handling.
- Listener tests verify malformed action events are dropped and the warning
  does not contain payload content.
- Run `cd ui; corepack pnpm run check`, `corepack pnpm run test:run`,
  `corepack pnpm run build`, and `cargo check --workspace --locked`.

## Rollback

Revert the UI mapper, its tests, this ADR, the architecture entry, and roadmap
update together. No Rust, database, configuration, or persisted data changes
are involved; no reset is required.
