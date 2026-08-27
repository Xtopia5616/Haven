---
description: Run static analysis: fmt, cargo check, clippy, svelte-check
agent: code
---
Run static analysis checks in order of speed.

If no args, run all:
  1. `cargo fmt --all -- --check 2>&1` (stop on failure)
  2. `cargo check --workspace --locked 2>&1`
  3. `cargo clippy --workspace --locked -- -D warnings 2>&1`
  4. `cd ui && pnpm exec svelte-check 2>&1`

With specific arg:
  fmt → only `cargo fmt --all -- --check`
  rust → only `cargo check --workspace --locked`
  clippy → only `cargo clippy --workspace --locked -- -D warnings`
  ui → only `cd ui && pnpm exec svelte-check`
