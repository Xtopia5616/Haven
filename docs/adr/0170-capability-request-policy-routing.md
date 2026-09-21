# ADR 0170: Capability and Request-Policy Model Routing

- Status: Accepted
- Date: 2026-09-19
- Owners: Haven maintainers

## Context

The persisted LLM configuration had five model slots (`small_model`,
`default_model`, `image_model`, `audio_model`, and `embedding_model`) plus
boolean switches for STT and vision. Adding a request type therefore required
another slot or another special case, and the meaning of a model depended on
which slot contained it. The settings UI and `get_api_key_status` also exposed
that fixed shape.

## Decision

- Persist named `llm.models` entries. Each entry references one provider,
  selects one provider model, and declares one or more `Capability` values.
- Persist ordered `llm.request_policies` entries. Each `RequestKind` has a
  primary model id and ordered fallback ids. The router accepts only assigned
  models whose provider credentials are usable and whose declared capability
  satisfies the request.
- Keep provider identity and wire protocol (`api_style`) separate from model
  capability. Provider adapters and their external wire contracts are
  unchanged.
- Make `RequestKind` the public router selector. Legacy role names are accepted
  only while loading old configuration and are converted once in memory; they
  are not serialized and do not define the configuration schema.
- Replace the STT/vision booleans with `transcription`, `audio_chat`, and
  `vision` policies. A model may serve multiple policies, so the same model
  assignment is not duplicated into slots.
- On load, convert an old `llm.roles` configuration once in memory. The five
  old names become model ids with inferred capabilities and equivalent
  request policies; the next settings save writes the new shape. No runtime
  database migration is needed.
- Expose API-key presence by named model id in `ApiKeyStatus.models`; provider,
  STT, and OCR status remain separate fields because they are different
  credential domains.

## Alternatives considered

- Adding more fixed slots was rejected because every new capability would
  expand the schema and router API again.
- Inferring capability from provider, model name, or `api_style` was rejected:
  those are unreliable hints and conflate transport compatibility with product
  capability.
- Keeping the booleans and letting the UI translate them was rejected because
  it would leave two routing sources of truth and make headless/runtime calls
  disagree with settings.
- Rewriting provider adapters was rejected. This change is Haven-internal
  configuration and routing semantics; adapter wire compatibility is already a
  stable boundary.

## Consequences

The configuration can express arbitrary named models, shared models, new
capabilities, and ordered fallbacks without adding another slot. A policy with
no eligible candidate is observably unconfigured and callers can degrade or
surface setup guidance. Production call sites use the `RequestKind` API or
resolved model ids, while old persisted role files are converted at load time.

The fixed-name migration code is intentionally limited to the configuration
loader. No compatibility selector is kept in the production router.

## Verification and rollback/reset

Coverage includes capability matching, shared model assignments, fallback over
an unconfigured primary, legacy TOML loading, router selection, STT behavior,
the dynamic settings contract, and the frontend model/policy editor. The
provider adapter suite remains unchanged and is run as part of `haven-llm`
tests.

Rollback is a source revert. Because this is a test-version configuration
contract change, a release containing this ADR must either run the one-time
loader conversion or reset `config.toml`; the loader conversion is deliberately
not a general historical migration system.
