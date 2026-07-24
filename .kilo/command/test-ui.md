---
description: Run UI (Svelte/Vite) tests with Vitest. Usage: /test-ui [--run|--coverage|--e2e] [filter]
agent: code
---
Run UI tests from the `ui/` directory.

If no args: `cd ui && npx vitest 2>&1` (watch mode).
--run: `cd ui && npx vitest run 2>&1`
--run <filter>: `cd ui && npx vitest run -- <filter> 2>&1`
--coverage: `cd ui && npx vitest run --coverage 2>&1`
--e2e: `cd ui && npx playwright test 2>&1`
--e2e:ui: `cd ui && npx playwright test --ui 2>&1`
