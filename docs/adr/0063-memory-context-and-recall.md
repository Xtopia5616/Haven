# ADR 0063: Typed memory recall and revision-keyed prompt context

## Background

`MemoryTool`, `InferenceEngine::recall_memory`, and
`SystemPromptBuilder::build_memory_sections` each performed their own JSON
mapping, sensitive filtering, and keyword/vector fallback. This made the
memory tool and desktop History path drift, and prompt refreshes repeated the
same retrieval after every dirty notification.

## Decision

- `haven_memory::recall` is the single typed read/retrieve boundary. It
  exposes `MemoryQuery`, `MemoryHit`, `MemoryRecall`, and `MemoryRetriever`.
- Fact visibility and episode text filtering happen in the memory boundary;
  callers cannot bypass them by consuming vector rows directly.
- Keyword and vector candidates are fused deterministically. Missing or empty
  vectors degrade to keyword recall; model filtering remains mandatory.
- `SystemPromptBuilder` caches rendered memory sections by query, embedding
  model, process-local memory revision, and excluded session id. The revision
  advances on fact, episode, and embedding mutations; it is not persisted and
  does not change the database schema.
- The embedding provider adapter only acquires a query vector. All vector rows
  are resolved through `MemoryRetriever`, which applies subject/session scope,
  legacy-row filtering, and typed normalization before any caller sees them.
- The shared callback returns `MemoryRecall`, preserving whether the result was
  keyword-only or hybrid; only the Tauri boundary projects `hits` into the
  existing `MemoryRecallItem[]` payload.
- Tauri keeps the existing `MemoryRecallItem[]` JSON shape and only converts
  from `MemoryHit` at the IPC boundary.

## Alternatives

Keeping JSON callbacks or letting each caller own retrieval was rejected
because both preserve duplicate policy and make sensitive filtering dependent
on the caller. Persisting a cache revision was rejected because the cache is
process-local and schema changes are unnecessary.

## Verification and reset

Focused coverage includes typed keyword/vector recall, sensitive legacy-row
filtering, empty-vector fallback, and prompt-cache invalidation after a memory
write. No database migration or data reset is required.
