# ADR 1052：统一 UI 静态词表命名并保留响应字段形状

## 状态

已接受（2026-10-12）

## 背景

UI 命名规范要求常量使用 UPPER_SNAKE_CASE，但部分跨模块导出的固定词表与选项列表使用 camelCase，且规范没有区分这些静态表和值/状态。`SessionLlmUsage` 也只把 `request_kind` 映射成 `requestKind`，其他 Rust-owned 响应字段继续保留 snake_case，形成同一行内不一致的字段口径。一个设置 contract 测试 fixture 模块使用 kebab-case 文件名，与 TypeScript 模块 camelCase 规范不符。

## 决定

- `docs/naming.md` 将 UPPER_SNAKE_CASE 的适用范围明确为静态词表与固定选项常量；UI 的固定工具结果词表、模型选项与媒体输入格式列表遵守此规则。局部值和运行时状态继续用 camelCase。
- resume usage 行保留生成 Rust DTO 的 `request_kind` 字段；不对单个响应字段做局部 snake_case→camelCase 转换。
- 测试 fixture 模块文件名使用 camelCase：`settingsFixture.ts`。

## 影响与兼容

词表导出绑定名称仅为 UI 源码内部重命名，值和 wire contract 均不变。resume 响应保留原有数据字段形状；设置 fixture 导入路径同步调整。无数据库迁移或外部 IPC shape 变化。

## 验证

- `corepack pnpm run check`
- `corepack pnpm run test:run`
