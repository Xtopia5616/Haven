# ADR 0299：ReAct compaction summary 通过 MemoryStore 持久化

- 状态：Accepted
- 日期：2026-09-25
- 范围：`ReActEngine` 的 raw Database ownership 收口、compaction episode summary 写入及 event-marker 调度
- 关联：[ADR 0296](0296-react-transcript-event-store-ports.md)、[ADR 0298](0298-react-event-boundary-cursor-session-store-port.md)、[ADR 0266](0266-summary-fact-extraction-durable-job.md)

## 背景

ReAct 的 transcript、event replay 与 event-boundary cursor 已通过 `SessionStore` 持久化。`ReActEngine` 仍保留 raw `Database` 字段用于写入 compaction summary episode，并按 summary 长度在同一事务创建 durable extraction marker；另外，branch-point 与 recovery-marker 写入虽然复用 `SessionStore` 的同步操作，Agent 仍在 raw Database blocking closure 中调度并检查 session 是否存在。

episode row 与 marker 的事务、冲突校验、幂等和 embedding cache invalidation 属于 Memory 持久层；summary trim、空值短路、长度策略、错误降级和提交后 live wake 属于 Agent 编排。两层不需要共享更宽的 Database facade。

## 决定

1. `haven-memory` 新增窄 `MemoryStore`，内部持有 `Arc<Database>`，并提供异步 `persist_compaction_summary(session_id, summary, episode_id, enqueue_extraction)`。该端口只接收已确定的 episode summary durable write 意图；它在 blocking pool 调用现有 `Database::add_episode_with_pending_extraction`，不引入 Agent 或 MemoryWorker 类型，也不改变底层事务、幂等、冲突错误或 cache invalidation 语义。
2. `SessionStore` 增加 branch-point 与 recovery-marker 的异步调度端口，在 blocking closure 内复用既有同步 append 操作和 session-existence 检查。缺失 session 的 branch-point 仍返回原错误；缺失 session 的 recovery-marker 仍成功 no-op。这只收口 ReActEngine 的 blocking 调度，不改变 event 事务或 marker 语义。
3. `ReActEngine` 删除 raw `Database` 字段，构造函数接收 `MemoryStore`。它继续 trim summary 并在 trim 后为空时短路；trim 后 `summary.len() >= 24` 才请求同事务写 pending extraction marker。端口失败仍记录 warning 并返回，不触发后续 wake；成功后仅在 marker 条件成立且 worker 存在时调用 `MemoryWorker::wake_summary_extract`。
4. `MemoryStore` 只拥有 episode 与 durable marker 写入。`MemoryWorker` 仍拥有 extraction 的 live wake、内存 outbox、恢复与消费；没有改变其其他 Database 路径。
5. `ReActEngine` 现在只依赖 `SessionStore` 与 `MemoryStore` 两个 Memory typed ports。`AgentLayer` composition root 从现有 `Arc<Database>` 创建并注入 `MemoryStore`；AgentLayer 仍因 MemoryService、MemoryWorker 等其他职责保留 raw Database。此改动不扩大到 resume attachment、MemoryWorker 其他 DB 路径或 schema。

把 Agent 的长度与 wake 策略移入 Memory 会混淆 domain ownership；让 ReActEngine 再次调度 raw Database 则会暴露事务与 blocking-pool 细节。窄 port 保留原调用顺序并隔离两种职责。

## 影响与验证

- 无 schema、IPC、持久数据契约或依赖变化，无需数据库重置。
- MemoryStore 测试覆盖 durable episode、pending marker、幂等重试、marker 重建及冲突事务回滚；SessionStore 测试覆盖 branch-point/recovery marker 的 session-existence 与 blocking 调度行为。
- ReAct 测试覆盖 trim/empty 短路、23/24 字节 extraction 边界、提交后 wake、持久化失败不 wake，以及失败后后续成功写入仍可工作；不依赖日志断言。
- 验收命令：`cargo fmt --all -- --check`、`cargo test --locked -p haven-memory`、`cargo test --locked -p haven-agent`、`cargo check --workspace --locked`、`cargo clippy --workspace --locked -- -D warnings`、`cargo test --workspace --locked`。

## 回滚

恢复 `ReActEngine.db` 与原有 compaction summary `run_blocking` 调用，删除 `MemoryStore` 新端口和对应测试、本 ADR、索引与路线图记录。无需迁移或重置数据库。
