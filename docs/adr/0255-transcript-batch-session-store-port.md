# ADR 0255：TranscriptBatchWriter 通过 SessionStore 持久化

- 状态：已采纳（2026-09-24）
- 范围：`haven-agent::TranscriptBatchWriter` 与 `haven-memory::SessionStore`
- 关联：[ADR 0207](0207-session-store-replay-boundaries-and-durable-ui-sequences.md)、[ADR 0251](0251-partial-stream-session-store-port.md)

## 背景

`TranscriptBatchWriter` 已经把事件与消息/步骤投影交给 `SessionStore`，但仍直接持有
`Arc<Database>` 来调度 blocking/cancellable SQLite 操作，并在 Agent 层重复检查 session
是否存在。这样 typed storage port 只收口了 SQL 语义，没有收口异步写入边界。

## 决定

1. `SessionStore` 提供 `append_transcript_batch_cancellable`，拥有阻塞调度、可选取消和
   缺少 session 行时返回空结果的兼容语义。
2. `TranscriptBatchWriter` 只持有 `SessionStore`；事件与投影事务仍唯一复用既有
   `append_transcript_batch` 实现，不新增 durable authority。
3. 空批次、缺少 session 行和正常批量写入在 SessionStore/Agent 边界补回归测试。

## 影响与验证

Agent 的 transcript 事件、投影提交顺序和取消行为不变；ReActEngine 其他 Database
依赖不在本切片内迁移。验证包括 fmt、workspace 严格 Clippy 和全 workspace 测试。

## 回滚

回退本切片提交并恢复 `TranscriptBatchWriter` 的 Database 调度即可；无需数据库重置。
