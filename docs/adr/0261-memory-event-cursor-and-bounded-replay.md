# ADR 0261：MemoryRuntime 事件游标与有界回放端口

- 状态：Accepted
- 日期：2026-09-24
- 范围：`haven-memory::SessionStore` 的 MemoryRuntime 基础持久化端口
- 关联：[ADR 0259](0259-memory-runtime-committed-event-consumer.md)

## 决策

为后续 MemoryRuntime 提供独立的 `memory_event_cursor.{session_id}` 时钟，并通过 `SessionStore` 暴露读取、单调 checkpoint、清理和最多 256 条的 durable event replay 分页接口。

该游标不复用 event projection cursor、`fact_extraction.{session_id}` message cursor 或 `last_msg_at`。checkpoint 拒绝回退；分页按 session 和 sequence 升序返回，并报告 `next_cursor` 与 `has_more`。现有 `subscribe_from` 的全量回放语义保持不变。

会话删除、清空、保留期删除和 orphan cleanup 同步清理该游标。此 ADR 只建立持久化边界，不实现事件 consumer、outbox-first 顺序、hook 触发替换、旧 session baseline 或 MemoryRuntime 启动编排。

## 验证

`haven-memory` 已覆盖默认/单调/隔离 cursor、分页顺序与边界、删除清理和 orphan cleanup；通过 Memory 定向测试、clippy、fmt 与 staged diff 检查。
