# ADR 0263：AgentLayer 的 MemoryRuntime 启动屏障

> 所有权与跨 crate handoff 后续由 [ADR 0367](0367-memory-runtime-application-ownership.md) 修订：ApplicationRuntime 执行并注册 prepare/live task，取得 typed `MemoryReady` 后调用 Agent dispatcher entry。下方 startup/replay 顺序与 fail-closed 行为继续有效；旧 AgentLayer `start*` 便利入口已删除。

- 状态：Accepted
- 日期：2026-09-24
- 范围：`haven-agent::AgentLayer` 启动编排与 `MemoryRuntime` 生命周期
- 关联：[ADR 0259](0259-memory-runtime-committed-event-consumer.md)、[ADR 0262](0262-memory-runtime-ordered-trigger-processing.md)

## 决策

`AgentLayer` 在 composition root 复用 `SessionSupervisor::session_store()` 与既有 `MemoryWorker` 创建唯一的 `MemoryRuntime`。不创建第二个 `Database`、`SessionStore` 或事实抽取 worker。

现有同步的 `start_with_cancellation` 与 `start_without_pending_recovery_with_cancellation` API 保持不变，但内部启动顺序统一为：

1. 订阅 live session event broadcast；
2. 为启动快照中的缺失 cursor 建立 baseline，并恢复 durable fact-extraction outbox；
3. 对已有 cursor 后的 durable events 做有界回放；
4. 启动 `MemoryRuntime` 的 live consumer；
5. 仅在上述准备成功后启动 SessionSupervisor dispatcher，选择是否恢复 pending sessions 仍由原入口决定。

启动准备失败会记录错误并放弃本次 dispatcher 启动；取消会安全退出，不推进 cursor、不清除 durable outbox marker。MemoryRuntime 的启动恢复继续使用 bounded replay 和 cursor 幂等，live/replay overlap 不产生重复入队。

本 ADR 不迁移 ReAct hooks 的 `InferCallback`，不改变 memory trigger payload，也不为同步 `start` API 增加可等待结果。下一片单独处理 hooks 只产生 typed trigger intent、由 committed event boundary 追加 `memory_trigger` 的迁移。

## 验证

Agent 测试覆盖两种 dispatcher 启动入口在 MemoryRuntime 未就绪时保持 pending、准备完成后继续调度；MemoryRuntime 测试覆盖已有 cursor 后的启动回放、显式零 cursor 回放、缺失 cursor 的旧 session baseline、bounded gap recovery、outbox restore 和 cancellation。通过相关 Agent 测试、clippy、fmt 与 staged diff 检查。
