# ADR 0175：恢复持久化使用 append-only 两阶段标记

## 背景

恢复失败路径同时写入 branch event、恢复消息、`session_steps` 投影和 snapshot。
这些写入共享 SQLite，但由不同的 repository 调用完成；任一步失败都不能再把
`partial_messages` scratch 当作已完成数据丢弃。此前调用方忽略了结构化结果，且
没有 durable 状态表示恢复持久化只完成了一部分。

## 决定

- 恢复写入开始前追加 `recovery_persistence: started` 控制事件；所有阶段完成后
  追加 `committed`，任一阶段失败则追加带阶段结果的 `failed`。
- 控制事件不进入 transcript projection，但保留在 append-only event stream 中，
  因此 snapshot 损坏或 rollback 后仍可诊断恢复失败。
- 只有终结标记成功且 branch point、恢复消息、projection、snapshot 全部成功时，
  才允许丢弃 scratch；上层显式消费 `RecoveryPersistenceResult`，失败路径保持
  session error 并保留 scratch。
- branch cutoff 的消息时间读取使用严格错误边界；读取失败时不写 branch point，
  防止用 `NULL` 代替未知 cutoff。

## 替代方案与影响

单次 SQLite 事务无法跨越异步 emitter 与现有 repository 边界；本 ADR 选择可恢复的
append-only 两阶段协议，保留已有事件 authority，不增加 schema 迁移。将来若把事件、
projection 与 snapshot 收敛到同一 repository，可把协议实现替换成单事务而不改变事件
语义。

## 验证与回滚

覆盖 recovery marker 的 durable replay、消息写入故障注入、严格 cutoff 读取失败、
以及恢复失败时 scratch 不被丢弃。回滚代码即可；不需要数据库重置。
