---
description: Run UI (Svelte/Vite) tests with Vitest. Usage: /test-ui [--run|--coverage|--e2e] [filter]
agent: code
---
Run UI tests from the `ui/` directory.

If no args: `cd ui && pnpm exec vitest 2>&1` (watch mode).
--run: `cd ui && pnpm exec vitest run 2>&1`
--run <filter>: `cd ui && pnpm exec vitest run -- <filter> 2>&1`
--coverage: `cd ui && pnpm exec vitest run --coverage 2>&1`
--e2e: `cd ui && pnpm exec playwright test 2>&1`
--e2e:ui: `cd ui && pnpm exec playwright test --ui 2>&1`
