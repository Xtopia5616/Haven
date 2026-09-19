# ADR-0169: Memory and Prompt Boundary Split

- Status: Accepted
- Date: 2026-09-19
- Owners: Haven maintainers

## Context

`SystemPromptBuilder` previously owned tool-index caching, memory recall,
embedding calls, SQLite reads, and prompt rendering. `InferenceEngine` also
owned fact extraction, the durable outbox, maintenance, and embedding-index
lifecycle. This made prompt refreshes and inference changes able to reach
database, router, and cache implementation details directly.

## Decision

1. `MemoryService` owns typed memory recall, bounded prompt candidates,
   embedding/index access, and the prompt-memory cache. It is shared by the
   prompt and background worker so a turn does not create duplicate index or
   cache state.
2. `MemoryWorker` owns asynchronous fact extraction, durable outbox draining,
   maintenance, and memory proposals/commits. `InferenceEngine` remains only
   as a source-compatible type alias for existing callers.
3. `PromptContextProvider` owns live tool/runtime context and the short
   capability-index cache. It obtains memory through `MemoryService`.
4. `PromptRenderer` is a stateless renderer for system prompts, memory fences,
   and bounded memory text. It accepts prepared values and does not access DB,
   routers, tools, or caches.

The existing prompt text, memory ranking, cache capacity, exclusion scope,
outbox durability, and embedding fallback behavior remain unchanged. The
shared `MemoryService` is constructed at the Agent composition root and passed
to both context and worker paths.

## Consequences

- Prompt code no longer stores a database/router/cache handle or performs
  embedding acquisition.
- Agent composition has one memory/index owner per runtime instead of separate
  prompt and inference index instances.
- Existing `InferenceEngine` callers can migrate incrementally; new code uses
  `MemoryWorker`.
- The memory worker still uses a narrow persistence capability for its durable
  extraction cursor and maintenance writes; those writes are not exposed to
  prompt rendering.

## Reset and rollback

No schema, durable key, prompt wire payload, or cache persistence contract
changed. Rollback is a source rollback only; no user database reset is needed.

## Verification

- `rustfmt --edition 2024 --check` on the changed Agent files
- `cargo check --locked -p haven-agent` (currently blocked by concurrent,
  unrelated uncommitted `haven-llm` adapter split in the working tree)
- `cargo test --workspace --locked` after the existing `haven-llm` working-tree
  compilation errors are resolved
