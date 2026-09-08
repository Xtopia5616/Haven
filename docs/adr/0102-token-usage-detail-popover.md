# ADR 0102：聊天 token 统计明细与上下文快照

- 状态：Accepted
- 日期：2026-09-08
- 范围：`ui/src/lib/SessionToolbar.svelte`、`ui/src/lib/sessionUsagePresentation.ts`、`haven-memory` usage projection

## 背景

聊天工具栏原先只显示会话累计 token，上传、生成、缓存命中率和当前上下文虽然已经由 `agent:usage` 部分提供，但主要藏在浏览器原生 tooltip 中。重新打开历史会话时，持久化用量也没有保存最后一次请求的上下文占用，因此无法解释当前上下文预算。

## 决定

1. token 摘要保留在工具栏，点击后打开明细 popover，统一展示当前请求的上传/生成/合计、当前上下文与窗口、缓存命中/未命中/写入、会话累计、费用、调用次数和模型。
2. 缓存命中率只在每次调用的 `inclusive` / `exclusive` accounting 已知时计算；跨 provider 的历史聚合如果存在 `unknown` 行则显示未知，不从数字猜测口径。
3. `llm_usage` 保存每次调用的 `context_tokens/context_window`，`session_usage` 保存按最后一次调用投影的上下文快照。schema v15 为旧数据库追加 nullable/default 列，旧调用的上下文显示为未知。
4. 恢复态优先使用最后一条持久化调用显示“当前请求”，累计值仍来自 session usage 聚合。

## 替代方案

- 继续扩展原生 tooltip：实现成本低，但不适合分组展示、键盘访问和移动窗口尺寸。
- 仅在前端从累计 prompt token 推算上下文：会把累计用量误当成单次请求上下文，且无法处理 cache-exclusive provider，因此不采用。

## 影响与回滚

- 新增一个向前 SQLite 迁移；回滚代码时必须保留 v15 读取兼容，或按项目发布说明备份并重建数据库。
- 没有新增敏感数据；持久化的字段仅为 provider 返回的 token 数和上下文窗口，不包含 prompt、cache key 或原始 provider 响应。

## 验证

- `cargo test --locked -p haven-memory`
- `cargo check --workspace --locked`
- `corepack pnpm run check`
- `corepack pnpm exec vitest run src/lib/SessionToolbar.test.ts src/lib/sessionUsagePresentation.test.ts`
