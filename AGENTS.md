# Haven Project Guide

## Project
Haven is a voice assistant for Windows PC built on the Pi Coding Agent (ReAct loop) architecture.
Tech stack: Rust (Tauri 2) backend, Svelte 5 frontend.

## Test Workflow

### Rust Backend
```sh
# Run all workspace tests
cargo test

# Run tests for a specific crate
cargo test -p haven-agent
cargo test -p haven-memory -- preferences

# Run with output
cargo test -- --nocapture

# Run with single thread (for DB-shared tests)
cargo test -- --test-threads=1

# Run clippy
cargo clippy -- -D warnings

# Coverage (requires cargo-tarpaulin)
cargo tarpaulin --out Html --output-dir target/coverage
```

### UI Frontend
```sh
cd ui

# Watch mode
npm run test

# Single run
npm run test:run

# With coverage
npm run test:coverage

# Svelte type check
npm run check
```

### Kilo Commands
- `/test [crate] [filter]` — run Rust tests
- `/test-ui [--run|--coverage|--e2e]` — run UI tests
- `/check [fmt|rust|clippy|ui]` — run static analysis
- `/coverage [--ci|--ui]` — generate coverage reports

## Cargo Aliases (via .cargo/config.toml)
- `cargo t` — `cargo test`
- `cargo ts` — `cargo test -- --nocapture`
- `cargo c` — `cargo check`
- `cargo cl` — `cargo clippy -- -D warnings`

## Test Conventions
- Use `#[cfg(test)] mod tests { ... }` in each source file for unit tests
- Use `crates/*/tests/` for integration tests
- Use `Database::open_in_memory()` for SQLite tests in haven-memory
- Mark test-only constructors with `#[cfg(test)]`
- Use `tokio::test` for async tests

## Before Committing
1. Run `/check` (fmt → check → clippy → svelte-check)
2. Run `/test` (all workspace tests pass)
3. Run `/test-ui --run` (UI tests pass)
