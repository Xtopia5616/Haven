# ADR 0460: Canonical Skill name validation

- Status: Accepted
- Date: 2026-10-04
- Related: ADR 0453, ADR 0459

## Context

Skill names flow into model-visible tool names and per-Skill virtual-environment
paths. The parser accepted arbitrary names while the venv manager sanitized
characters by replacing them with underscores. Distinct names could therefore
share a tool identity or venv directory. Windows device names and
case-insensitive aliases also made otherwise accepted names unsafe or
ambiguous.

## Decision

1. Define one shared Skill-name validator in `haven-skills`: names contain 1–128
   ASCII letters, digits, `-` or `_`, and Windows device names are rejected
   case-insensitively.
2. Apply the validator when parsing manifests, creating virtual environments,
   and creating Skills through the administration API. Use the validated name
   directly as the venv directory component; do not lossy-sanitize it.
3. During directory scanning, skip every Skill in a case-insensitive directory
   name collision or manifest-name collision. This keeps tool identities and
   venv paths unambiguous on Windows.
4. Document the accepted format and recovery action for existing invalid
   manifests. Renaming a Skill does not affect database schema; update the
   `[skills].enabled` allowlist when applicable.

## Impact and verification

Invalid manually-authored Skills are skipped during scanning, and Skills with
case-only name collisions are all skipped. Existing valid names keep their
venv directory names. Tests cover validator boundaries, parser rejection,
collision handling, venv path preservation and rejection before filesystem
changes, and the administration create path.

## Rollback

Reverting this change restores permissive manifest names and lossy venv path
sanitization. Any Skill renamed to satisfy this policy can be renamed back, but
doing so may restore a collision or make it unavailable on Windows.
