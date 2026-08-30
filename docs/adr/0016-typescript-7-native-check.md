# ADR 0016：使用 TypeScript 7 原生编译器进行 UI 类型检查

## 背景

Haven UI 当前使用 Svelte 5、SvelteKit 2 和 `svelte-check` 4。TypeScript 7 已提供原生编译器，但 Svelte 检查链仍通过 TypeScript 编程 API 生成和分析虚拟文件；这些 API 仍由 TypeScript 6 提供，直接把 `typescript` 替换成 7 会破坏 `svelte-check` 与 SvelteKit 的兼容性。

## 决定

- 增加 `@typescript/native`，通过 npm alias 锁定 TypeScript 7.0.2。
- 保留 `typescript` 6.0.3，作为 Svelte 工具链的兼容 API 层。
- 将 UI 的 `check` 脚本切换为 `svelte-check --tsgo`，由 TypeScript 7 原生编译器执行生成代码的项目类型诊断。
- `.kilo/command/check.md` 只调用该脚本，保证本地与 CI 使用相同的检查入口。

## 替代方案

直接升级 `typescript` 到 7 会让 Svelte 工具链使用尚未兼容的 TypeScript 7 编程 API；继续只使用 TypeScript 6 则无法完成 TS7 迁移。因此不采用这两个方案。

## 影响

类型检查采用 TS7 的诊断结果，构建与运行时行为不变。依赖安装会额外锁定当前平台的 TypeScript 7 原生包；Svelte 相关工具仍明确依赖 TS6，待其公开 API 兼容 TS7 后可移除兼容层。

## 验证

- `corepack pnpm install --frozen-lockfile`
- `corepack pnpm run check`
- `corepack pnpm run test:run`
- `corepack pnpm run build`

## 回滚

回退 `ui/package.json`、`ui/pnpm-lock.yaml`、`.kilo/command/check.md` 和本 ADR，然后重新执行 `corepack pnpm install --frozen-lockfile`。该变更不修改数据库、配置或应用数据。
