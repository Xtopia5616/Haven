---
description: Run static analysis: fmt, cargo check, clippy, svelte-check
agent: code
---
Run static analysis checks in order of speed.

If no args, run all:
  1. `cargo fmt --check 2>&1` (stop on failure)
  2. `cargo check 2>&1`
  3. `cargo clippy -- -D warnings 2>&1`
  4. `cd ui && npx svelte-check 2>&1`

With specific arg:
  fmt → only `cargo fmt --check`
  rust → only `cargo check`
  clippy → only `cargo clippy -- -D warnings`
  ui → only `cd ui && npx svelte-check`
