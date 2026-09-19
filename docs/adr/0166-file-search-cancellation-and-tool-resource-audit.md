# ADR-0166: Cancellable File Search and Resource-Aware Tool Views

- Status: Accepted
- Date: 2026-09-18
- Owners: Haven maintainers

## Context

`files.search` performed blocking directory traversal and content scans inside a
Tokio blocking task. A large file with no match produced no sink callbacks, so
the cancellation token was not observed until EOF. The outer tool timeout could
return a timeout result while the scan continued consuming blocking workers.
Operation views also replaced aggregate resource locks with `ReadOnly`, which
allowed file reads/searches to overlap file mutations.

The process listing path independently refreshed all system data even though it
only needed the process table.

## Decision

1. Content search uses a bounded cancellation-aware reader and disables memory
   maps explicitly, so cancellation is observed between bounded reads even for
   no-match files.
2. Search result admission uses an atomic reservation counter. The shared JSON
   result lock is only taken for results that fit under the cap.
3. The shared file-search engine admits one full-tree scan at a time; waiting
   for that slot is cancellation-aware.
4. Read-only operation views preserve the aggregate's resource key. File
   reads, outlines, summaries, and searches share the `files` resource with
   file mutations.
5. Process listing refreshes only processes instead of calling `System::new_all`.

## Consequences

- Search cancellation releases its blocking worker promptly after the current
  bounded read; repeated timed-out searches no longer silently accumulate work.
- Search result collection has less lock contention after the result cap is
  reached.
- Separate sessions no longer multiply internally parallel scans against the
  same blocking pool and disk.
- File reads and searches no longer race with writes from the same tool batch.
- Process listing avoids unrelated disk, network, hardware, and user refreshes.

## Verification

- `cargo test --locked -p haven-tools file_search`
- `cargo test --locked -p haven-tools cancellable_reader_stops_after_cancel`
- `cargo test --locked -p haven-tools file_read_views_share_the_files_resource_with_writers`
- `cargo fmt --all -- --check`
