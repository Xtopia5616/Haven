# ADR 0350: Session UI field mapping audit

> Resume camelCase alias、coercion 与默认字段分支已由 [ADR 0376](0376-session-ui-obsolete-compatibility-removal.md) 删除；本 ADR 的 mapper 边界判断仍有效。

- Status: accepted (2026-09-25)
- Scope: session lifecycle event mapping, session reducer modules, resume projection helpers, and session command response consumers
- Related: [ADR 0330](0330-session-lifecycle-ui-contract-mapper.md), [ADR 0346](0346-app-event-listener-contract-boundary.md), [ADR 0347](0347-agent-event-contract-validation-boundary.md), [ADR 0349](0349-session-terminal-cleanup-audit.md)

## Context

Phase 8 left the handwritten session DTOs and field projections for a focused audit. The audit traced Rust session event DTOs and command responses through `contracts/session.ts`, `events.ts`, `chatEventController`, the session reducer modules, `resumeMessages.ts`, `continueSession.ts`, and the resume command consumers.

## Findings

- `contracts/session.ts::mapSessionEvent` is the only Rust snake_case to UI camelCase mapper for session lifecycle events. Both session listener APIs in `events.ts` share it; the chat event controller and handlers consume its camelCase DTOs.
- `sessionReducer/types.ts`, `state.ts`, and `lifecycle.ts` define or update reducer-owned state. They do not repeat lifecycle wire mapping. `transcript.ts` updates in-memory message state; `resumeMessages.ts` projects persisted transcript rows into chat bubbles; `usage.ts` projects the separate snake_case aggregate usage response into token statistics. These projections have different input and output types from lifecycle events.
- `get_sessions` returns `SessionInfo`; `+page.svelte` maps its `waiting_reason` to `waitingReason` once before dispatch. The reducer's lifecycle updates copy already normalized values and do not map the wire field again.
- Live `interaction:requested` events are mapped by `contracts/app.ts::mapAppEvent`. Resume responses take a separate path through `sessionReducer/interaction.ts::resumeInteractions`, which validates unknown rows, accepts both snake_case wire names and camelCase compatibility names, applies resume-specific coercion/defaults, and drops rows without IDs. The paths produce `InteractionRequest`, but their validation and compatibility policies are not interchangeable; folding them into the lifecycle mapper or a universal field mapper would add policy branches or change behavior.
- `continueSession.ts` chooses behavior from renderer messages already projected by `resumeMessages.ts`; it has no Rust DTO field mapping.

## Decision

- Keep the lifecycle event mapper, live interaction event mapper, resume interaction compatibility normalizer, transcript projection, and usage projection at their current boundaries.
- Do not add a shared session mapper or a generic snake_case/camelCase helper: the audit found no duplicate session DTO mapping with identical input shape and normalization policy.
- Add regression coverage for resume interactions in both wire and camelCase compatibility shapes, including rejection of malformed rows and envelopes.
- Keep Rust DTOs, IPC payloads, event order, reducer transitions, event deduplication, X12 event/projection semantics, and session behavior unchanged.

## Alternatives

- Fold resume interaction normalization into `mapSessionEvent`: rejected because it handles a different DTO and command-response compatibility policy.
- Share one configurable mapper between live and resumed interaction inputs: rejected because their validation, optional-field handling, and defaults differ; policy flags would add a generic mapping layer without removing the distinct boundary responsibilities.
- Introduce Rust-to-TypeScript code generation: outside this audit slice; no codegen flow is added.

## Impact

Only regression coverage and architecture records change. Runtime mappers, command/event contracts, Rust DTOs, and renderer behavior remain as before.

## Verification and rollback

Passed with Node.js 24.20.0 and pnpm 11.24.0:

- `corepack pnpm --dir ui run check`
- `corepack pnpm --dir ui run test:run`
- `corepack pnpm --dir ui run build`
- `pwsh -NoProfile -File scripts/check-ipc-events.ps1`
- `pwsh -NoProfile -File scripts/check-ipc-contracts.ps1`
- `git diff --check`

The regression test and this audit record can be reverted directly. No Rust, wire, schema, persisted state, or reset changes are involved.
