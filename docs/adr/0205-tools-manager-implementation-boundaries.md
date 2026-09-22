# ADR-0205: Split ToolsManager Implementation by Runtime Boundary

- Status: Accepted
- Date: 2026-09-22

## Context

`crates/tools/src/lib.rs` had become the implementation and test home for the
`ToolsManager` facade. Its code mixed composition/wiring, catalog and
session-overlay discovery, execution/authorization policy, retry helpers, and
the complete manager regression suite. The existing `ToolCore`, `ToolRuntime`,
and `ToolBuiltins` objects already express the intended ownership, but the
crate root obscured those boundaries and made changes harder to review.

## Decision

Keep `ToolsManager` as the stable facade and split its implementation into
three internal modules:

- `manager.rs` owns construction, dependency wiring, runtime capability
  accessors, managed-asset lifecycle, and startup/configuration updates;
- `catalog.rs` owns MCP/Skill discovery, session overlays, catalog rebuilds,
  provider-budget selection, and catalog projections;
- `execution.rs` owns tool execution entry points and execution-facing policy
  queries.

The crate root retains module declarations, shared private helpers, and the
public re-exports. Manager tests move to `tests.rs`. No tool name, IPC shape,
authorization rule, persistence format, or runtime behavior changes.

This is an internal module boundary, not a new crate: the existing
`ToolCore`/`ToolRuntime`/`ToolBuiltins` composition remains the dependency
direction, and `ToolsManager` remains the narrow application-facing facade.

## Consequences

- `lib.rs` is reduced to 379 lines; each production implementation module is
  below the hotspot threshold.
- Catalog and execution changes can be reviewed independently from startup
  wiring and managed-asset lifecycle changes.
- The larger architectural follow-up remains: remove service-locator-shaped
  responsibilities from the facade where explicit capability ports can replace
  them. This ADR does not claim that cross-crate redesign is complete.

## Verification

- `cargo check --locked -p haven-tools`
- `cargo test --locked -p haven-tools --lib`
