# ADR 0405: Current Config Contract Without Migration

- Status: accepted (2026-09-29)
- Related: ADR 0401, ADR 0403, `docs/development-standards.md`, `docs/release-and-reset.md`

## Context

Haven is a fast-moving test build and does not promise backward compatibility for `config.toml`. The loader had accumulated special paths that moved `[memory].history_retention_days`, scanned removed tool and permission names, and imported plaintext provider, OCR, and MCP credentials into the secure credential store. These paths made the loader a second schema definition and could preserve settings whose old meaning no longer matched the current contract.

## Decision

1. Parse `config.toml` directly as the current `AppConfig`. Remove pre-parse normalization, legacy-name detection, and migration-time rewriting. Missing fields that are optional in the current schema continue to use their declared defaults.
2. Reject unknown fields in config structs, including nested sections and `Settings`. A removed field or section invalidates the whole file rather than being silently discarded. Dynamic maps such as `tool_settings` remain dynamic current-schema extension points; no old key is renamed or reinterpreted.
3. Do not import plaintext API keys, OCR secrets, or MCP environment values from TOML. Persist only opaque secure-store references. Startup hydrates runtime values from those references; a missing reference value is reported and the user must enter the credential again.
4. Keep invalid-file recovery: copy the original to a timestamped `.bak`, use defaults in memory, and leave the source file unchanged during startup. This is recovery, not an automatic migration. Resetting only `config.toml` is sufficient when the database and other user data should remain.
5. Keep validation for current semantic constraints, such as canonical provider wire styles and media provider references. Invalid persisted permission keys remain ignored by the authorization engine, which fails closed; they are not converted.
6. `[session].history_retention_days` remains the current field. The config conversion described in ADR 0403 is superseded; the session ownership and cleanup decisions in ADR 0403 remain in effect.

## Alternatives

- Keep one-time migrations for common old fields and credential values: rejected because they maintain a parallel old schema and silently preserve data across intentionally breaking changes.
- Silently ignore unknown fields: rejected because it hides stale settings and can make an old policy appear to have been applied.
- Delete all recovery behavior: rejected because retaining a recoverable copy of malformed user configuration prevents accidental loss and does not provide compatibility semantics.

## Effects and reset

An old configuration may stop loading after an update. Haven preserves it as a timestamped backup and starts with in-memory defaults; it does not rewrite the old file on startup. Users can inspect the backup, delete only `%APPDATA%\haven\config.toml` (or `~/.local/share/haven/config.toml` outside Windows), restart, and re-enter current settings. This leaves the database, logs, media, and skills in place. Old plaintext secrets may be present in the backup and require careful handling.

Rolling back the code alone does not restore a secret already stored in the operating-system credential manager. A rollback that must restore prior behavior requires the matching application revision and the corresponding configuration/profile backup.

## Validation

- Config loader tests verify that the old retention location, removed model fields, removed root sections, and plaintext provider/MCP values are backed up and rejected without changing the source file.
- Credential tests verify that startup hydrates only current secure references.
- Run `cargo fmt --all -- --check` and the applicable `haven-common` tests/checks; see the implementation review for the executed results.
