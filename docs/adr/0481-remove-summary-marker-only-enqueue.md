# ADR 0481：移除单独写入 summary marker 的旧入口

- 状态：已采纳（实现进行中）
- 日期：2026-10-05
- 范围：`haven-agent`、`haven-memory` 的 compaction summary extraction marker 写入入口
- 关联：[ADR 0259](0259-memory-runtime-committed-event-consumer.md)、[ADR 0266](0266-summary-fact-extraction-durable-job.md)、[ADR 0299](0299-react-compaction-summary-memory-store-port.md)、[ADR 0301](0301-memory-worker-outbox-through-memory-store.md)、[ADR 0448](0448-remove-unreferenced-memory-apis.md)

## 背景

正式生产路径由 `ReActEngine::persist_compaction_summary` 调用 `MemoryStore::persist_compaction_summary`，在一个 SQLite 事务中写入 episode 与可选 pending marker；事务成功后再调用 `MemoryWorker::wake_summary_extract` 更新 live outbox。durable marker 是恢复权威，live outbox 只是即时唤醒投影。

`MemoryWorker::enqueue_summary_extract`、`MemoryStore::enqueue_summary_extraction_cancellable` 与 `Database::enqueue_summary_extraction` 仍允许在 episode 已存在后单独写 marker，但当前没有 workspace 生产调用者，只有旧入口测试使用。该旁路绕过 ADR 0266 的 episode/marker 同事务约束，也与 ADR 0259 Phase 7.2 移除旧 callback 的方向不一致。

## 决定

1. 删除上述三个 marker-only enqueue API，不保留 source-compatibility wrapper。Haven 尚未承诺稳定的 Rust 下游 API；这三个 crate 均为 `0.1.0`，仓库搜索没有生产调用方。
2. marker 的创建只经 `Database::add_episode_with_pending_extraction`，由 `MemoryStore::persist_compaction_summary` 在 production 调用。Agent 按提交结果调用 `wake_summary_extract`；失败不发布 live wake。
3. 保留 `pending_summary_extractions`、`clear_summary_extraction`、episode 内容读取、启动恢复、live wake、worker retry/cancel、逐 episode ack 与 session cleanup。schema、marker key/value、恢复语义与 summary 准入门槛不变。
4. 将低层 marker 测试 fixture 改为调用 episode+marker 原子入口，以继续覆盖多个 episode 独立恢复/ack 与 session 删除清理；删除只证明旧 marker-only API 可写入的测试。ReAct producer 的准入、同事务提交和提交后 wake/失败不 wake 测试继续保留。

## 替代方案

- 保留公开 marker-only API 供未来调用：拒绝。它没有当前生产消费者，会提供第二个 durable writer，并允许 episode 与 marker 分开提交。
- 将 marker-only API 改为转发到 episode 原子 API：拒绝。调用方没有 episode 正文，兼容 wrapper 无法满足原子契约。
- 删除 pending/read/ack API：拒绝。这些入口仍由 worker 恢复、消费和确认路径使用。

## 影响与验证

- 收窄 Agent/Memory Rust source API；没有 workspace 外的稳定兼容承诺。没有 schema、持久数据、ID、X12、IPC 或配置变化，无需重置数据库。
- 验证旧三个 API 无全仓调用残留，并保留 ReAct producer 边界、episode/marker 幂等与冲突回滚、独立 ack、session cleanup、worker 恢复及取消/失败保留 marker 的行为覆盖。
- 本切片跨 crate 且移除持久化写 API，运行 workspace fmt、测试、check 与严格 Clippy；具体结果在实现完成后回填。

## 回滚

可恢复此前 Rust 方法及其实现，无需数据重置。若恢复 marker-only writer，必须同时撤销本 ADR 的唯一 marker 创建约束，并说明如何避免 episode 与 marker 分开提交。
