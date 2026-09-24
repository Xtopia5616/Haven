# ADR 0269：MemoryWorker 的应用停机边界

- 状态：Accepted
- 日期：2026-09-24
- 范围：MemoryWorker durable outbox 的进程内生命周期
- 关联：[ADR 0266](0266-summary-fact-extraction-durable-job.md)、[ADR 0268](0268-memory-outbox-retry-backoff.md)

## 背景

`MemoryWorker` 的 durable fact/summary outbox 由一个 lazy `tokio::spawn` 的 live
projection drain loop 消费。它不属于 `ApplicationRuntime` 的 task registry；如果应用只取消
ReAct 或 maintenance task，worker 仍可能在停机期间继续调用 provider，甚至在 durable marker
尚未安全完成时继续处理新任务。

## 决策

`MemoryWorker` 持有自己的 shutdown `CancellationToken`，并由 `AgentLayer` 暴露一个应用停机
入口。`ApplicationRuntime::shutdown` 在取消应用根 token 后显式调用该入口。worker 在 durable
outbox restore 后、空队列等待、每个 job 的推理边界和 retry/backoff 等待处响应取消；停机时
同时取消 prompt prefetch，并唤醒 outbox `Notify`。

取消不会清除事实或 summary marker，也不会为尚未完成的 live job 写成功确认。当前 batch 被
丢弃只意味着 live projection 停止；marker 是恢复权威，下一进程会重新 restore。已经完成推理
但尚未确认 marker 的 job 依靠既有 extraction cursor/idempotent 写路径安全重放。

该切片不把 outbox worker 改造成通用 `ActionService` job，不新增跨领域 lease/cancel/UI
状态，也不强制中断已经进入 provider 的单次请求；provider 返回后 worker 会在 ack 前再次
检查取消。ApplicationRuntime 仍负责其他 app task 的 join，MemoryWorker 的 durable marker
负责跨进程恢复。

## 验证

- worker 单元测试覆盖 shutdown token 已取消以及取消后不会再次启动 outbox worker；
- 既有 durable marker、summary recovery、MemoryRuntime startup 和 application shutdown 测试继续通过；
- 通过 fmt、严格 Clippy、workspace tests 和 staged diff 检查。
