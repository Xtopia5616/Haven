---
description: Run Rust workspace tests. Usage: /test [crate] [test-filter] [--nocapture] [--no-fail-fast]
agent: code
---
Run Rust tests with `cargo test` in the workspace root. 

If no arguments: run `cargo test 2>&1`.
If first arg is a crate name, map it to the package:
  common → -p haven-common
  memory → -p haven-memory
  tools  → -p haven-tools
  action   → -p haven-action
  agent  → -p haven-agent
  llm    → -p haven-llm
  input  → -p haven-input
  desktop → -p haven-desktop
  app    → -p haven-app-binary
Then add any remaining args as a filter (`-- <filter>`).

Special flags (pass through to cargo test):
  --nocapture, --no-fail-fast, --release, --doc

Do NOT cd into crate directories — always run from workspace root.
