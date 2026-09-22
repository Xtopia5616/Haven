# ADR-0204: Remove Internal Compatibility Layers During Rapid Iteration

- Status: Accepted
- Date: 2026-09-22

## Context

Haven is still changing quickly. Internal aliases, source-compatible trait
defaults, old configuration migrations, legacy checkpoint imports, and
multiple wire shapes increase the number of states that every caller and test
must support. That cost is no longer justified while the local data model and
UI contracts are still being reshaped.

## Decision

Internal callers use one current contract. Remove compatibility paths that
only preserve Haven's previous implementation, including:

- renamed Rust APIs and executor aliases;
- optional reducer state fields and single-item stream action aliases;
- old configuration backup names and model-routing migrations;
- importing transcript or branch data from an old snapshot when the durable
  event log is absent;
- legacy snapshot wire fields and wrapper/partial tool manifests.

Old local configuration, checkpoint, and database state must be reset or
rebuilt instead of being migrated at runtime. Tests seed the current durable
event stream explicitly and construct complete current state shapes.

This decision does not remove adapters required by external protocols. MCP,
provider APIs, and other user-facing wire formats remain normalized at their
boundaries because those are active integration contracts, not Haven's old
internal representations.

## Consequences

- Internal code has fewer branches and stronger compile-time contracts.
- A development build may require a local reset after these contracts change.
- Regressions in the current event, manifest, and reducer shapes fail close to
  their source rather than being silently accepted by a compatibility shim.
