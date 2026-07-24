---
description: Generate test coverage reports. Usage: /coverage [--ci|--ui]
agent: code
---
Generate code coverage reports.

No args: `cargo tarpaulin --out Html --output-dir target/coverage 2>&1`
--ci: `cargo tarpaulin --out Lcov --output-dir target/coverage 2>&1`
--ui: `cd ui && npx vitest run --coverage 2>&1`

If `cargo-tarpaulin` is not installed, advise: `cargo install cargo-tarpaulin`
