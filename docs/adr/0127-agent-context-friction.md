# ADR 0127: Reduce model-visible context friction

## Background

Runtime review showed that the model could receive large repeated grouped-tool
schemas, a full file read could stop without a resumable position, optional
capabilities could remain advertised after they became unavailable, and empty
memory results had no explicit meaning. These increase unnecessary tool turns
and make the next action ambiguous.

## Decision

- Keep public grouped tool names unchanged, but compact object-root provider
  projections: the flattened root owns field definitions and dependent
  branches retain only discriminator, required, and nested validation structure.
- Add `files.outline` as a bounded, line-numbered structural read. Full text
  reads now return `next_offset` whenever the returned prefix is truncated.
  Character budgets count Unicode scalar values consistently.
- Emit an explicit `MEMORY: (none)` block with `reason: no_hits` for an empty
  prompt recall, and return a typed `empty_reason` for memory-tool recalls.
- Rebuild the model-facing catalog from live availability: omit `load_skill`
  and `load_mcp` when their indexes are empty, and omit media/TTS operations
  whose backends are not configured.
- Include the detected workspace root and effective context/tool caps in the
  runtime snapshot. Increase the default content-search snippet to 640
  characters while retaining the global observation budget.

## Alternatives

Splitting every grouped tool into new public provider tool names would reduce
schema size further, but would require a coordinated renderer, permissions,
and persisted-contract migration. Intent-based heuristic filtering was also
rejected because it could silently hide a capability needed later in a task.

## Impact

The changes are backward-compatible for persisted sessions and do not change
database schema. A provider sees fewer duplicate schema descriptions and an
agent can continue bounded reads from a stable cursor. The catalog may no
longer expose an extension loader when no enabled extension exists; enabling
or discovering one rebuilds the catalog. The outline scanner stays in its own
bounded module so the existing `files` dispatcher remains the stable aggregate
boundary; it is intentionally a heuristic structural hint, not a language
parser.

## Verification and rollback

Verified with targeted schema, Unicode, outline, memory-empty, and
capability-filtered tests, plus the full Rust workspace and UI gates. Rollback
is a code and prompt change only; no database or configuration reset is
required.
