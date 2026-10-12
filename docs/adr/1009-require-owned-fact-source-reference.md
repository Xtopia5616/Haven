# 1009：FactSourceRef 必须引用规范消息身份

## 状态

已采纳并实施（2026-10-12）。

## 背景

UI 的 Fact response parser 要求 `source_ref.message_id` 是 canonical `msg-*`，但 Memory `FactGraph` 写入时允许空 `message_id`。这会存下孤立 snippet，后续查询将其映射成缺少有效 owner 的 source reference，导致 Memory 命令返回值被 UI 拒绝。

## 决定

- `source_ref: Some(...)` 必须带 canonical `msg-*`；空值应通过 `None` 表达，不把“只有 snippet”的对象作为 source reference 写入。
- 查询投影若没有 provenance message ID，则忽略遗留孤立 snippet，并返回 `source_ref: None`。
- UI parser 保持现有 canonical ID 校验，形成写入、读取与 renderer 的同一契约。

## 影响

SQLite schema 与列不变。新写入拒绝空/非法 source ID；历史孤立 snippet 不再作为 source reference 显示，之后可由既有 orphan cleanup 清理。没有数据库重置要求。

## 替代方案

- 允许空 ID 并放宽 UI parser：拒绝；snippet 不能定位来源消息，破坏可追溯性。
- 读取时虚构 message ID：拒绝；身份只能由消息 owner 生成。

## 验证

- `cargo test --locked -p haven-memory fact_graph::tests`
- `cargo test --locked -p haven-memory fact_query::tests`
- `corepack pnpm exec vitest run src/lib/memoryCommands.test.ts`
- `cargo clippy --workspace --locked -- -D warnings`

## 回滚

可恢复对空 source ID 的接受及 snippet-only 读取投影；这会重新产生 UI 与 Memory response contract 不一致的结果。
