# ADR-0101: Tool Contract and Result Renderer Audit

- Status: Accepted
- Date: 2026-09-08
- Owners: Haven maintainers

## Context

The tool suite had several operation families whose input schema was broader than
the operation they represented. Some successful results also lacked a stable
operation discriminator, which forced the UI to infer behavior from incidental
fields or fall back to a generic JSON view. These gaps made regressions look like
the UI had reverted to its original renderer and made tool contracts harder to
validate at the model boundary.

## Decision

1. Admin tools expose operation-scoped strict schemas. Each operation declares
   only its accepted fields, while `oneOf` branches express operation-specific
   required fields and the exact tool/resource allowlists.
2. Input results include an `operation` discriminator. Result renderers use the
   discriminator and dedicated renderers for memory, input, audio, schedule,
   window, and Haven administration results; unknown shapes retain a safe JSON
   fallback.
3. Long provider-facing MCP/Skill names are kept deterministic and collision
   resistant within the provider limit by adding a short SHA-256 suffix instead
   of silently truncating two distinct names to the same value.
4. Contract changes are covered by Rust schema/unit tests and UI parsing,
   rendering, and fallback tests. The renderer boundary remains centralized in
   `toolResultRenderers.ts` so new result types cannot accidentally bypass the
   dedicated UI path. The old `ToolResultCard` parser re-export is removed;
   callers import the parser module directly.

## Consequences

- Invalid cross-operation parameters are rejected before execution.
- Tool results are more readable and preserve operation-specific affordances.
- Provider tool names remain within the 64-character limit without predictable
  truncation collisions.
- Adding a new result family requires updating the parser/renderer registry and
  its tests, rather than relying on field-shape guesses scattered across cards.

## Verification

- `cargo test --workspace --locked`
- `cargo clippy --workspace --locked -- -D warnings`
- `corepack pnpm run test:run`
- `corepack pnpm run check`
- `corepack pnpm run build`
