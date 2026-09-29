# ADR 0403: Session-Owned Attachment Retention

> The config-key migration described in Decision 6 was superseded by [ADR 0405](0405-current-config-contract-without-migration.md). Session ownership and retention behavior remain current.

- Status: accepted (2026-09-29)
- Related: ADR 0113, 0115, 0118, 0119, 0121, 0374

## Context

Committed user uploads under `default_work_dir()/uploads/file-{uuid32}/` were collected by directory age using the session-history retention duration. This could keep files after their session was manually deleted, and directory modification time did not identify which session owned an attachment. Generated media under `default_generated_media_dir()` had a separate seven-day expiry; an attachment could therefore become unavailable while its session remained in history. The process-local managed-asset registry already has pending ingress and per-session leases for the interval before durable message projection and while a session is active.

Whole-session history retention is a session lifecycle policy, so its setting is owned by `[session]`, alongside the other session lifecycle controls. The old `[memory].history_retention_days` location obscured that ownership and made media cleanup appear to be a memory subsystem policy. The original implementation moved the old key before parsing; ADR 0405 removes that conversion and requires users to recreate incompatible configuration.

## Decision

1. A managed attachment's durable owner is the session message whose `messages.ui_metadata.attachment_previews` entry stores its host-owned path. The path reference set from `SessionStore::list_managed_attachment_paths` is authoritative for cleanup. A path shared by multiple messages or sessions remains available until its final durable reference is removed. Do not infer sharing from bytes or compare file contents.
2. Pending-ingress and active-session registry leases remain independent protection. They protect files during the upload-to-session handoff, event-to-message projection, and active tool use. Explicit session delete/clear releases that session's lease only after the durable deletion succeeds.
3. A committed upload batch is removed as soon as it contains no durable reference and no active/pending lease. Its modification time no longer extends or shortens attachment lifetime. The host sweeper only visits direct children with the exact `file-{uuid32}` directory name under the dedicated uploads root; it refuses symlink/reparse roots and entries. Unknown names and non-directory entries are skipped.
4. Generated files that are session-bound follow the same durable-reference and lease rules. Registering a session-owned asset suppresses its independent `expires_at`, so a retained session can still use it; any surviving message reference protects the file even if its persisted attachment metadata contains a past expiry. A generated file with no session reference, lease, or still-live detached runtime TTL is an orphan and is removed during reconciliation without an mtime grace period. The cleaner only visits strict `file-{uuid32}.{extension}` regular files directly under the dedicated generated-media root and skips symlinks/reparse points. Detached runtime-only registrations keep their explicit TTL while registered in the process. After restart their in-memory asset identity and TTL are gone, so an unreferenced generated file is an orphan and is removed by the next sweep.
5. The media reconciliation API requires a successfully read reference set. A database/reference error fails closed and leaves media untouched. Startup and daily retention work delete expired sessions first, then read references and reconcile both media roots. Explicit delete/clear commands reconcile after the durable delete. The sweep also runs when automatic session retention is disabled so manual deletions and crash-orphaned committed uploads are reclaimed.
6. The session retention duration is configured as `[session].history_retention_days` (default 90 days; `0` disables automatic session deletion). It controls deletion of whole sessions and their durable history; attachment files then follow the references left by surviving sessions. This replaces the old `[memory].history_retention_days` config location. No config conversion is performed; an old `[memory]` key is rejected with the rest of an incompatible config file.
7. Upload staging directories remain a separate crash-cleanup concern: strict `.file-{uuid32}.tmp` directories retain their 24-hour startup/daily cleanup and are not protected by session references. The upload write lock covers both batch commits and cleanup; a pending lease is registered before the lock is released after rename.

These rules supersede the age-based committed-upload retention in ADR 0115, the completed-upload retention note in ADR 0118, and ADR 0119's independent lifetime for session-attached generated media and its restart-time seven-day mtime fallback. Durable message references and live leases still protect generated files; detached generated media keeps its bounded runtime TTL only while its process-local registration remains available.

## Security and failure behavior

Cleanup accepts no renderer/model path as a deletion target. It uses only the two host-owned roots, strict generated names, no-follow metadata checks, and direct-child traversal. The database reference query must succeed before either root is swept; malformed persisted attachment metadata therefore blocks cleanup instead of risking deletion. Shared references and live leases always win over cleanup. Deletion failures are logged with sanitized errors and do not stop processing other files.

## Compatibility, validation, and reset

This changes host filesystem lifecycle and moves the current history-retention setting from `[memory]` to `[session]`; it adds no database table/column, attachment-specific config option, IPC payload, or schema version. The config loader does not migrate the legacy key; see ADR 0405 and the release/reset document. This decision introduces no database reset requirement; any reset required by other schema changes in the same release is governed by the release/reset document. Focused regression coverage verifies shared durable references, lease release, pending upload handoff, session-bound generated expiry suppression, unowned generated cleanup, strict naming, and staging's independent age policy.

Rollback consists of reverting the reference-driven media sweeper and the session-lease release hook together. This may leave unreferenced managed files for later cleanup; it does not require database recovery.
