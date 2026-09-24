# ADR 0301：MemoryWorker outbox 持久化通过 MemoryStore

- 状态：Accepted
- 日期：2026-09-25
- 范围：`MemoryWorker` durable fact/summary outbox marker 的读写与 episode text 读取
- 关联：[ADR 0266](0266-summary-fact-extraction-durable-job.md)、[ADR 0268](0268-memory-outbox-retry-backoff.md)、[ADR 0269](0269-memory-worker-shutdown-boundary.md)、[ADR 0299](0299-react-compaction-summary-memory-store-port.md)

## 背景

ADR 0299 将 compaction summary episode 与初始 extraction marker 的原子写入收口到 `MemoryStore`，但 `MemoryWorker` 仍直接通过 `MemoryDatabase` 调度 outbox marker 的 enqueue、restore、ack，以及 summary episode 读取。这样 durable outbox 的写入 owner 与 live job owner 仍混在 Agent 的 Worker 调度代码中。

Marker 的 SQL、条件确认、episode 查询和 blocking-pool/SQLite 取消属于 Memory 持久层。Worker 的内存队列、live wake、事实推理、逐 job retry/backoff 与停机生命周期仍属于 Agent。fact extraction 算法本身及其 transcript/fact/cursor 数据访问、maintenance、kv 和 embedding 路径不属于本切片。

## 决定

1. `MemoryStore` 增加窄 cancellable ports，覆盖 fact marker enqueue、fact/summary pending marker 查询、fact marker 条件 ack、summary marker enqueue/ack 和 episode text 查询。每个异步端口都在 `run_blocking_cancellable` 中调用既有 `Database` 方法，不改变返回类型、底层错误或 marker SQL 语义。
2. 保留无 Tokio runtime 的 `enqueue_infer` 同步 fallback。该分支通过 `MemoryStore::enqueue_fact_extraction_without_runtime` 写 durable marker，再按既有 API 行为加入 live map；它不创建 runtime，也不持有新的数据库连接。
3. `MemoryService` 持有 `MemoryStore`，并向 `MemoryWorker` 和 ReAct composition root 提供共享 clone。各 clone 与 `MemoryService` 使用同一个 `Arc<Database>`；不创建第二个连接或独立 facade。
4. `MemoryWorker` 的 enqueue、显式 restore、worker 启动 restore、fact job ack、summary episode read 和 summary marker ack 全部使用 `MemoryStore`。成功 ack 才移除 live retry 状态；marker 写入失败继续保留 durable row 并按原退避策略重排队。取消停止 live projection，不清理 marker。
5. `MemoryWorker` 保留 `MemoryDatabase` 兼容句柄，供本切片明确未迁移的 fact extraction algorithm 数据读写、maintenance、kv cursor/throttle 和 embedding 路径使用。本切片不改 transcript/fact algorithm、inference、embedding 或 maintenance policy；上述句柄不再用于 durable outbox marker 或 summary episode 读取。

`MemoryStore` 负责 durable outbox，`MemoryWorker` 负责 live projection、inference 与 retry/backoff。这里不引入通用 job 状态机或 Agent 类型依赖。

## 影响与验证

- 无 schema、IPC 或 Cargo 依赖变化，无需数据库重置。
- MemoryStore 测试覆盖新端口的成功、参数/数据库错误、条件 ack、缺失 episode 与 SQLite 写入取消；MemoryWorker 测试覆盖成功 ack、marker ack 失败后的 durable 保留与 live retry，以及 inference 期间取消后的 marker 保留。
- 不以日志作为测试断言。
- 验收命令：`cargo fmt --all -- --check`、`cargo test --locked -p haven-memory`、`cargo test --locked -p haven-agent`、`cargo check --workspace --locked`、`cargo clippy --workspace --locked -- -D warnings`、`cargo test --workspace --locked`。

## 回滚

恢复 `MemoryWorker` 对应 outbox 路径中的原 Database blocking 调度，删除本 ADR、README 索引、路线图记录、MemoryStore 端口与回归测试。无数据迁移或重置要求。
