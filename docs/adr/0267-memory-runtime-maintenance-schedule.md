# ADR 0267：MemoryRuntime 拥有 maintenance 调度策略

- 状态：Accepted
- 日期：2026-09-24
- 范围：周期 memory maintenance 的调度职责
- 关联：[ADR 0259](0259-memory-runtime-committed-event-consumer.md)、[ADR 0266](0266-summary-fact-extraction-durable-job.md)

## 背景

`ApplicationRuntime` 已经拥有后台任务的取消和 join，但 `app_state.rs` 同时硬编码了
memory maintenance 的六小时 interval、首次 tick 和失败日志。这样应用组合根需要知道
Memory 领域的调度策略，`MemoryRuntime` 又只负责 committed event consumer，领域边界不完整。

## 决策

将六小时周期和“首次 tick 立即执行、失败只记录并继续下一周期”的策略移动到
`MemoryRuntime::run_maintenance_until_cancelled`。它调用既有 `MemoryWorker::run_memory_maintenance`
执行完整维护 pass；不改变 dedup、sensitive purge、contradiction、低置信度清理、embedding
catch-up 或 LLM 仲裁的顺序和返回语义。

`ApplicationRuntime` 仍创建一个 app-scoped task，提供 child cancellation token、持有 join handle
并负责 shutdown。`AgentLayer` 只提供 app-facing 的 schedule facade；手动 Tauri command 继续
调用单次 `run_memory_maintenance`，不经过周期循环。周期维护错误不会进入 ReAct turn。

本切片不把 `MemoryWorker` 的执行逻辑迁入 `MemoryRuntime`，不新增 `MemoryReader`，也不把
maintenance 做成用户可见 `ActionService` job。后续若需要通用 Job 生命周期，必须另行定义
claim/lease/retry/cancel 和 UI projection 契约。

## 验证

- MemoryRuntime 取消测试确认已取消的 schedule 在首次 tick 前退出。
- 既有 MemoryWorker maintenance 测试继续覆盖执行顺序和失败隔离。
- 通过 agent/app focused tests、workspace tests、fmt、Clippy 和 staged diff 检查。
