---
description: Run UI (Svelte/Vite) tests with Vitest. Usage: /test-ui [--run|--coverage|--e2e] [filter]
agent: code
---
Run UI tests from the `ui/` directory.

If no args: `corepack pnpm --dir ui exec vitest 2>&1` (watch mode).
--run: `corepack pnpm --dir ui exec vitest run 2>&1`
--run <filter>: `corepack pnpm --dir ui exec vitest run -- <filter> 2>&1`
--coverage: `corepack pnpm --dir ui exec vitest run --coverage 2>&1`
--e2e: `corepack pnpm --dir ui exec playwright test 2>&1`
--e2e:ui: `corepack pnpm --dir ui exec playwright test --ui 2>&1`
