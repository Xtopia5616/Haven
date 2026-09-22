# ADR 0202：会话用量汇总采用增量投影

- 状态：Accepted
- 日期：2026-09-22
- 范围：`haven-memory` 用量持久化

## 背景

`llm_usage` 是逐次调用的明细表，旧的追加路径每写入一条明细就对整个会话执行
一次 `SUM`，导致同一会话的连续调用产生随历史长度增长的写放大。`session_usage`
仍需要作为恢复态的快速读取投影，但不应在普通追加路径重复扫描历史明细。

## 决定

- 追加 LLM 用量时，在与明细插入相同的 `BEGIN IMMEDIATE` 事务内把 Agent 用量增量
  应用到 `session_usage`；工具/媒体明细仍保存，但不进入 Agent 汇总。
- `session_usage.updated_at` 同时作为上下文快照的时间水位。只有更晚的 Agent 明细
  才能替换 `context_tokens/context_window`，因此阻塞写入乱序不会回退上下文快照。
- rollback、truncate、按 id 删除后的补偿路径继续从 `llm_usage` 全量重建汇总；明细
  仍是可验证和可恢复的权威来源。
- 批量工具用量只执行一次汇总增量更新，不按明细条数重复更新或扫描历史。

## 替代方案

每次追加都从明细查询汇总实现更简单，但会使追加成本随会话长度增长。完全取消
`session_usage` 并在每次恢复时查询明细也能避免双写，但会让恢复和工具栏读取承担
不必要的聚合成本；当前保留持久化投影以满足低延迟恢复。

## 影响与回滚

本决定不改变数据库列或 wire 契约，不需要数据库重置。若回滚代码，恢复追加路径的
全量重建即可；`session_usage` 可由现有 rebuild 方法重新校正。

## 验证

- `cargo test --locked -p haven-memory -- usage`
- `cargo test --locked -p haven-memory -- repositories::messages::tests::truncate_session_after_cleans_llm_usage_rows`
