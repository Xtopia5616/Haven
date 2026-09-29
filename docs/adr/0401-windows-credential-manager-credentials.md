# ADR 0401: Store Credentials in Windows Credential Manager

- Status: accepted (2026-09-29)
- Related: ADR 0405, `docs/development-standards.md`, `AGENTS.md`

## Context

Provider API keys, OCR credentials, and MCP environment values are secrets. Keeping them in `config.toml` exposes them to ordinary file reads, backups, and accidental config or debug serialization. Settings and MCP management also need to distinguish a configured credential from a missing one without returning its value to the renderer.

## Decision

1. Store credential values in Windows Credential Manager using generic credentials. `haven-common` defines the `CredentialStore` port and validates opaque `cred-{uuid32}` references; `haven-platform` implements the Windows adapter. The persisted reference is an identifier, not a secret.
2. TOML retains provider/OCR identity and endpoint settings with credential references. MCP TOML retains environment variable names, `has_value`, and references. Store every MCP environment value, including empty and apparently non-sensitive values, so security does not depend on variable-name heuristics. Name-only MCP entries remain names without a credential reference.
3. Settings writes stage a secret to secure storage first, return only its reference, then save that reference in TOML. MCP add/update is an explicit secret-entry path; it also writes values to the store before the config edit completes. A secure-store write error is returned to the caller and prevents the corresponding config save. Settings snapshots and read-oriented MCP/admin results omit runtime secret values; MCP confirmation input displays only names and `[redacted]` markers.
4. Runtime code resolves references from the credential store. A configured reference with no stored value fails closed with an observable error. Non-Windows builds may start with no configured references, but any configured credential reference or attempt to stage/write a secret fails visibly because no persistent secure backend is available. In-memory storage is limited to explicit tests and development helpers; it is not a production persistence fallback.
5. Do not migrate plaintext credentials automatically. A config containing an old plaintext provider key, OCR secret, or MCP environment value is rejected and backed up according to the current config reset boundary in ADR 0405. The user resets/recreates the current config and re-enters credentials through Settings/MCP management. A generated backup can still contain the old plaintext, so users must protect or securely delete it after recovery.
6. Credential reference IDs use `haven_common::types::new_id("cred")`; `cred-` is recorded in the persistent ID prefix table in `AGENTS.md`.

## Alternatives

- Keep secrets in TOML and rely on file ACLs: rejected because ordinary backups, diagnostics, and future serialization paths would still handle plaintext.
- Store only environment variables whose names look sensitive: rejected because token and password names are inconsistent and the heuristic would be easy to bypass accidentally.
- Use process-local storage on non-Windows as a fallback: rejected because references would stop resolving after restart.
- Automatically import old plaintext: rejected under the no-compatibility policy in ADR 0405; users explicitly re-enter credentials after config reset.

## Effects and reset

On Windows, secrets persist independently from `config.toml`; deleting the config file does not remove Credential Manager entries. Unreferenced staged values are discarded when a Settings edit is canceled where possible. If a write or cleanup fails, the command reports the failure and logs only sanitized error text. Resetting config requires re-entering credentials, and deleting old timestamped backups may be necessary to remove plaintext copies.

## Validation

- Unit tests cover reference validation, missing-reference failure, write failure, Settings/MCP output redaction, and plaintext provider/OCR rejection.
- Windows adapter tests and workspace checks should run on Windows CI; non-Windows builds verify the explicit unavailable-backend implementation compiles.
