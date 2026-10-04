# ADR 0459: Persist Skill requirements fingerprints

- Status: Accepted
- Date: 2026-10-04
- Related: ADR 0453

## Context

`VenvManager` previously kept each Skill's `requirements.txt` checksum only in
memory. A new manager is created after every application restart, so its empty
cache made the first `ensure` run `pip install` again even when the Skill's
requirements had not changed.

## Decision

1. Compute a stable SHA-256 fingerprint from the requirements file contents.
2. Store the fingerprint in the corresponding virtual environment after
   `pip install` succeeds.
3. Skip installation when the stored fingerprint matches. A missing or
   different fingerprint triggers installation; failed installation never
   updates the marker and remains retryable.
4. Existing virtual environments without a marker install their requirements
   once and then gain the marker.

## Impact and verification

This changes only per-Skill cache metadata. It does not change database,
configuration, or IPC contracts. The marker contains a fingerprint, not
requirements or secret material. Tests cover reuse across manager instances,
changed requirements, and retry after failed installation.

## Rollback

Reverting this change restores the prior per-process checksum behavior.
Removing `.haven-requirements-fingerprint` from an existing Skill venv forces
the next `ensure` to install that Skill's requirements once.
