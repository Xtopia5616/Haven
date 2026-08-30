# ADR 0017：升级 Node.js 到 24.20.0 LTS

## 背景

项目原先固定在 Node.js 24 的较早 LTS 补丁版本；Node.js 24 分支现已发布 24.20.0 LTS。当前 UI 依赖链（Vite、SvelteKit、Vitest 与 TypeScript 7 原生编译器）均已在 Node 24 上验证通过。

## 决定

- 将 `.node-version` 与 `ui/package.json` 的 `engines.node` 精确固定到 24.20.0，确保本地开发、CI 与 UI 依赖链使用同一 Node.js 补丁版本。
- CI 继续通过 `.node-version` 读取版本，不新增独立版本来源。
- pnpm 继续固定为 11.24.0；不因 Node 自带 npm 版本变化而引入第二套包管理入口。

## 替代方案

暂不跳到 Node 26。Node 26 当前仍处于 Current，Node 24 是生产项目适用的 LTS 分支；跨主版本升级需要单独完成原生依赖与桌面打包验证。

## 影响

开发者和 CI 需要使用 Node 24.20.0。该升级不改变应用运行时 API，也不修改数据库、配置或用户数据。

## 验证

- `corepack pnpm install --frozen-lockfile`
- `corepack pnpm run check`
- `corepack pnpm run test:run`
- `corepack pnpm run build`

## 回滚

回退 `.node-version`、`ui/package.json`、相关规范/README 文档和本 ADR，然后重新执行 `corepack pnpm install --frozen-lockfile`。不需要数据重置。
