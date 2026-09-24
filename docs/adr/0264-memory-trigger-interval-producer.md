# ADR 0264：Interval Memory Trigger 的 durable producer

- 状态：Accepted
- 日期：2026-09-24
- 范围：`haven-agent` ReAct interval memory trigger producer
- 关联：[ADR 0259](0259-memory-runtime-committed-event-consumer.md)、[ADR 0263](0263-memory-runtime-agentlayer-startup-barrier.md)

## 决策

把 ReAct `before_step` 中按步数触发的事实抽取从直接调用 `MemoryWorker::enqueue_infer` 改为 typed intent：`DefaultHooks` 只返回共享的 `MemoryTriggerPayload`，不持有或调用 worker。payload 与 `MemoryRuntime` 共用同一序列化类型，wire event 仍为 `memory_trigger`。

`TurnEngine` 在 `before_step` 成功后、provider request 发出前，通过既有 `SessionStore` 在 blocking pool 追加 durable trigger。此时上一边界的 transcript 已提交，trigger sequence 会排在已提交历史之后；事件包含 `run_id` 与 `step_number`。写入失败或取消只记录 warning/debug，不改变原有 ReAct turn 的成功、失败和取消语义；合成的无 session 测试路径不产生写入。

本切片只迁移 interval trigger。`on_pause` 的 Ask、Confirm、TurnEnd、Budget、External 路径仍保留旧 `InferCallback`，下一片再把 pause intent 接到各自的 durable event boundary。MemoryPatchHandle、summary extraction、recall 和 rollback 语义不在本 ADR 内。

## 验证

覆盖 payload 的 event type、sequence、run_id、step_number、取消、写入失败和合成 session；hook 在 interval 命中时返回 typed intent 且不调用 legacy callback。通过 Agent focused tests、clippy、fmt 与 staged diff 检查。
