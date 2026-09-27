# ADR 0377：Memory outbox 删除同步 enqueue 兼容入口

- 状态：已采纳（2026-09-27）
- 范围：Agent `MemoryWorker` fact extraction enqueue 与 MemoryStore marker 写入端口
- 关联：[ADR 0301](0301-memory-worker-outbox-through-memory-store.md)、[ADR 0266](0266-summary-fact-extraction-durable-job.md)

## 背景

`MemoryWorker::enqueue_infer` 通过 Tokio runtime 探测在异步 durable enqueue 与同步 SQLite fallback 之间分支。当前生产代码只使用严格的 `enqueue_infer_durable`；旧入口没有生产调用方，只有单测仍依赖它。它在 durable 写失败时仍会把任务放入 RAM 队列，和 durable outbox 的权威语义冲突。

## 决定

1. 删除未使用的 `enqueue_infer` 双路径入口以及 `MemoryStore::enqueue_fact_extraction_without_runtime` 同步端口。
2. 测试与生产调用统一使用 `enqueue_infer_durable`：只有 durable marker 写入成功后才更新进程内 outbox；失败由调用方获得错误，不创建仅存在于 RAM 的工作。
3. 不改变 worker 的 retry/backoff、marker ack、取消或恢复流程。

## 影响、重置与验证

- 没有 schema、配置、IPC 或持久化格式变化，不需要重置用户数据。
- 用 MemoryWorker 测试覆盖 durable marker 先于 RAM 投影及 bypass flag 合并；运行 Agent 与 Memory crate 测试。

## 回滚

恢复同步端口和旧 runtime 检测入口及对应测试；无需数据迁移。
