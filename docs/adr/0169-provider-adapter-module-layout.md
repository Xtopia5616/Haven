# ADR 0169: Provider Adapter Module Layout

- Status: Accepted
- Date: 2026-09-19
- Owners: Haven maintainers

## Context

The four primary chat adapters in `haven-llm` had each accumulated provider
wire DTOs, request construction, response parsing, stream event handling,
canonical mapping, feature decisions, transport calls, and protocol tests in a
single source file. Shared transport and framing already existed, but provider
files still mixed protocol details with orchestration and regression coverage.

## Decision

Each primary provider module is organized into the following internal slices:

- `wire.rs`: provider DTOs, event types, and serde details;
- `request.rs`: URL/body construction and calls into the existing shared transport;
- `response.rs`: non-stream response parsing and the Gemini embedding parser;
- `stream.rs`: provider event interpretation; shared SSE/JSONL framing remains in `adapters::stream`;
- `mapping.rs`: canonical-to-provider and provider-to-canonical mapping;
- `features.rs`: capability profiles and provider-specific thinking/cache/vendor decisions;
- `tests.rs`: the existing provider regression matrix;
- `tests/fixtures` and `golden.rs`: redacted request, response, and stream protocol fixtures.

The public factory and `LlmClient` boundary remain unchanged. OpenAI Chat,
OpenAI Responses, Anthropic, and Gemini wire JSON, stream semantics, errors,
capabilities, and round-trip state remain unchanged. Shared transport,
framing, embedding, web search, and provider feature policy remain owned by the
existing cross-provider modules.

## Alternatives considered

- Keeping one file was rejected because protocol edits and test-only changes
  had an unnecessarily large review surface.
- Splitting each provider into a new crate was rejected because the current
  boundaries are internal and do not justify new crate-level dependencies.
- Introducing a common wire DTO layer was rejected because provider payloads
  are intentionally different and the canonical mapping boundary is the safer
  place to normalize them.

## Consequences

Provider protocol changes now have smaller review surfaces and explicit homes.
Shared policy remains shared instead of being duplicated in adapters. The old
flat source paths are removed, but this is a source-only reorganization: no
configuration, database, cache, session, or external API migration is needed.

## Verification and rollback

The change is verified with formatting, workspace compilation, clippy, the
`haven-llm` provider regression suite, and provider-local golden fixture tests.
Rollback is a source-only revert; no persisted data migration is involved.
