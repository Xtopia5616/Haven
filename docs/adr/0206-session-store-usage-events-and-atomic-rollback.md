# ADR 0206：SessionStore 统一用量事件投影与回滚边界

- 状态：Accepted
- 日期：2026-09-22
- 范围：`haven-memory`、`haven-agent` 会话持久化

## 背景

`timeline_rollback` marker 与 `messages`、`session_steps`、`llm_usage` 投影
曾由不同提交完成；崩溃窗口会让事件流和投影暂时不一致。与此同时，Agent
actor、tool usage 和 media usage 仍可直接调用 `Database` 的用量写入 API，绕过
SessionStore 的事件边界。

## 决定

- `SessionStore::rollback_to` 在一个 `BEGIN IMMEDIATE` 事务内解析回滚游标、追加
  `timeline_rollback`、截断三类投影并重建 `session_usage`；提交后才广播 marker。
- 每次模型调用写入 `usage_recorded` 域事件，并在同一事务投影到 `llm_usage` 与
  `session_usage`。Agent、tool、media 三条写路径都只调用 SessionStore。
- 发生回滚 epoch 竞态或仅投影截断时，写入 `usage_discarded` 补偿事件并删除投影
  行，避免后续事件重放重新生成已丢弃的用量。
- `llm_usage` 与 `session_usage` 仍是读取优化投影；事件 payload 保存完整的
  `LlmCallUsage`，不改变现有 IPC shape 或数据库 schema。

## 替代方案

保留 Database 直写并只在回滚后重建汇总，无法消除 event/projection 双写边界；
仅删除 usage 投影而不写补偿事件，则在事件重放或未来投影修复时会复活已丢弃行。

## 影响、重置与回滚

本决定不新增 schema 列，不要求数据库重置。回滚实现时必须同时恢复 Agent 的
三条 SessionStore 写路径和 usage 事件补偿，否则不能保证事件重放与投影一致。

## 验证

- `cargo test --locked -p haven-memory -- repositories::session_events::tests::usage_events_and_rollback_projection_share_one_transaction`
- `cargo test --locked -p haven-memory -- usage`
- `cargo check --locked -p haven-agent`
