# ADR 0126: Align the model contract with live runtime state

## Background

The agent prompt and tool boundary could describe one contract while the
runtime supplied different facts: private execution metadata was re-injected
into strict provider arguments, observations were capped before a useful source
window fit, and the prompt did not expose the host defaults and optional model
capabilities that control tool behavior.

## Decision

- Keep private execution metadata out of provider-facing tool arguments unless
  the tool explicitly consumes that field (`_session_id` / `_step_id`). The
  durable step id is not injected as `_idempotency_key`; idempotency is a
  runtime policy, not a serde argument.
- Set the default observation budget to 32,000 characters and expose
  continuation metadata (`next_start_line`, `next_offset`) on bounded reads.
- Decode text through the existing UTF-8, UTF-16, and GBK fallback chain while
  returning a stable `encoding` label from file reads.
- Put a bounded runtime snapshot in SESSION CONTEXT: host identity, cwd and
  shell defaults, local time/locale, model/media/MCP/Skill availability, and a
  non-sensitive permission summary.
- Keep the static prompt concise. Read-only calls do not require a preamble,
  extension loading is conditional on an advertised backend, and the old
  imperative closer is retained only for legacy snapshots.

## Alternatives

Splitting every grouped tool into separate provider tools would reduce schema
size further, but it changes the public tool names and renderer/security
matrix together. It remains a separate migration; this ADR first fixes the
runtime and prompt contracts without renaming existing tools.

## Impact

The change is backward-compatible for persisted prompts: cache-boundary and
memory-patch code recognizes the legacy closer. Existing settings with an
explicit observation limit are preserved; only the default changes.

## Verification and rollback

Verified with targeted `haven-common`, `haven-tools`, and `haven-agent` tests;
the strict-argument regression test proves the private idempotency field does
not reach `deny_unknown_fields` deserialization. The change can be reverted as
one code/doc commit; no database or config reset is required.
