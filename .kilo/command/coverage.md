---
description: Generate test coverage reports. Usage: /coverage [--ci|--ui]
agent: code
---
Generate code coverage reports.

No args: `cargo tarpaulin --locked --out Html --output-dir target/coverage 2>&1`
--ci: `cargo tarpaulin --locked --out Lcov --output-dir target/coverage 2>&1`
--ui: `cd ui && pnpm exec vitest run --coverage 2>&1`

If `cargo-tarpaulin` is not installed, advise: `cargo install cargo-tarpaulin`
