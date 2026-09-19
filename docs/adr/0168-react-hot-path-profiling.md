# ADR 0168: Remove ReAct Hot-Path Rework

- Status: Accepted
- Date: 2026-09-18
- Owners: Haven maintainers

## Context

Profiling the ReAct turn preparation path found three avoidable costs:

1. The per-session token estimate cache serialized the complete canonical
   history into a SHA-256 digest on every check. The digest protected against
   same-length replacements, but it made the steady-state append path O(history)
   before token estimation even started.
2. `RequestContext` deep-copied the canonical messages and then copied them
   again while applying the common raw-media capability projection. Stream setup
   also re-ran the token estimate over that request-only copy.
3. Automatic inbox delivery awaited a blocking file-lock claim every polling
   cadence. A concurrent sender or explicit inbox operation could therefore
   delay the next model turn for the full lock timeout.

## Decision

- Give each in-memory canonical projection a monotonic revision. The single
  transcript projection boundary updates append estimates by message cost;
  non-append edits increment the revision and cause one safe full rebuild.
  Cache validation no longer serializes the full history.
- Store request messages and media metadata behind `Arc` and keep the exact
  provider-visible message token count on `RequestContext`. Capability
  selection returns a shared request for complete, notice-free raw projections
  and only clones for an actual fallback/rewrite. Retry instructions calculate
  the incremental cost of their new message.
- The provider stream retry loop materializes one immutable message/tool
  snapshot and passes `Arc` handles to every attempt. The four production
  streaming adapters consume the shared boundary as borrowed slices while
  constructing their wire request, so retries do not rebuild the canonical
  message/tool vectors. The compatibility default on `LlmClient` remains
  available for third-party/test adapters that still expose owned arguments.
- ReAct state maintains a compact index of media-bearing transcript events.
  Request-context media identity recovery walks that index and updates it on
  append/compaction, so long text/tool histories do not force a full event-log
  scan merely because the current request contains media.
- Add `try_claim` to the messaging transport boundary. The JSONL transport
  attempts the lock once (recovering one stale lock) and returns busy without
  sleeping. Automatic ReAct polling uses this path; explicit inbox and
  request/reply operations retain the normal bounded blocking claim semantics.

## Consequences

- Normal turn-start estimate checks are O(1) after the initial estimate and one
  tokenization pass is still performed after a replacement or in-place edit.
- Raw media turns avoid a second canonical-message allocation; media fallback
  behavior and durable asset identity are unchanged.
- A busy automatic inbox poll is deferred to the next notification/cadence.
  At-least-once claim/ack semantics remain unchanged because a claim is still
  held until transcript and snapshot durability.
- The revision is an in-memory invariant. Resume/rebuild starts at revision
  zero and safely repopulates the cache on its first check.

## Validation and rollback

- `cargo check --locked -p haven-agent -p haven-tools`
- `cargo test --locked -p haven-agent -- --nocapture`
- `cargo test --locked -p haven-tools -- --nocapture`
- Regression tests cover same-length token-cache replacement, append reuse,
  request-media fallback, immediate return while the inbox lock is held, and
  retry attempts reusing the same immutable provider snapshot. A long-history
  media replay test verifies that only media-bearing event indexes are walked.
- Revert this ADR's implementation commit to restore digest validation,
  deep-copy request projection, and blocking background claims; no database or
  on-disk reset is required.
