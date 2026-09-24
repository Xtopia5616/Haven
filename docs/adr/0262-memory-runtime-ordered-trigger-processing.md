# ADR 0262：MemoryRuntime 已提交触发的顺序处理核心

- 状态：Accepted
- 日期：2026-09-24
- 范围：`haven-agent::MemoryRuntime` 与现有事实抽取 outbox
- 关联：[ADR 0259](0259-memory-runtime-committed-event-consumer.md)、[ADR 0261](0261-memory-event-cursor-and-bounded-replay.md)

## 决策

新增 `MemoryRuntime::process_event` 作为后续 live/replay consumer 的唯一事件处理核心。调用方按 session 串行传入已提交事件；核心拒绝 sequence gap，按 `memory_event_cursor` 幂等跳过已处理事件，并只解析 `memory_trigger` 的 typed payload。

处理 `memory_trigger` 时严格按以下顺序执行：

1. 校验当前 event version、`trigger_kind` 与 `bypass_throttle` 的组合；
2. 通过 `MemoryWorker` 把现有 `fact_extraction_pending.{session_id}` durable marker 写成功；
3. 将任务放入现有内存 outbox；
4. checkpoint `memory_event_cursor`。

普通事件只推进 memory event cursor，不触发事实抽取。任何 payload、outbox、checkpoint 或 cancellation 错误都不推进 cursor；checkpoint 失败会留下 durable marker，允许后续重放。普通任务完成 ack 不会清掉并发到达的 `bypass=true` 升级。

本切片不负责订阅 broadcast、分页补 gap、启动恢复、旧 session baseline、dispatcher 启动顺序或 ReAct hooks 改造；这些仍由后续接入切片完成。旧 `MemoryWorker::enqueue_infer` 的 best-effort 行为保留，只有 Runtime 使用严格可 await 的 durable enqueue 入口。

## 验证

覆盖普通事件、合法 interval/pause trigger、重复/升级、坏 payload、outbox/checkpoint 故障、sequence gap、跨 session 隔离和 cancellation-after-enqueue；通过 Memory/Agent 测试、两 crate clippy、fmt 与 staged diff 检查。
