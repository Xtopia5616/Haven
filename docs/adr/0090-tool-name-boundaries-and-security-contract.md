# ADR 0090: Close tool-name boundaries and registry security contracts

> Superseded for legacy-name handling by [ADR 0100](0100-remove-tool-name-compatibility.md).

- Status: Accepted
- Date: 2026-09-06

## Context

Haven's public tool names are `files` and `schedule`, but older configuration
and session projections can contain `file`, `file_search`, or
`scheduled_action`. The model-facing `haven_tools` capability also exposes
tool enable/disable operations, so an unconstrained target name could change
admin or progressive-loader tools. The previous security matrix checked only
representative rows and could drift from the actual builtin schemas.

## Decision

1. The legacy-name handling in this item is superseded by ADR 0100. The
   remaining security matrix and capability allowlist decisions stay active.
2. Preserve `process.launch` as an unknown historical name and mark its card
   unrecoverable instead of guessing a replacement.
3. Restrict model-driven `haven_tools` toggles to an explicit allowlist of
   ordinary execution tools. Admin capabilities and `load_skill`/`load_mcp`
   remain protected; native UI/admin calls keep their existing surface.
4. Require the security matrix to match the actual builtin registry families,
   schema-declared routing operations, risk levels, and permission keys. The
   test creates the registry with admin capabilities enabled and fails on any
   missing, extra, or drifted row.

## Alternatives considered

- Dropping all old config/history rows would avoid migration code but silently
  lose permanent grants and renderer fidelity.
- Allowing every `tool_settings` name and relying only on confirmation risk
  would leave management tools mutable by model output.
- Keeping only representative security rows was smaller but did not protect
  against schema additions or operation/risk drift.

## Impact and rollback

The next config load rewrites old permission keys in memory and subsequent
saves persist the canonical names. Existing history remains readable; only
`process.launch` gets an explicit non-recoverable marker. If the allowlist or
matrix needs correction, update the single map/table and its tests; no
database reset is required.

## Verification

- `cargo test --locked -p haven-common -- config::loader types`
- `cargo test --locked -p haven-tools -- security builtin::admin`
- `corepack pnpm --dir ui run check`
- `corepack pnpm --dir ui run test:run`
