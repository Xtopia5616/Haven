# ADR 0265：Pause Memory Trigger 的 durable producer

- 状态：Accepted
- 日期：2026-09-24
- 范围：`haven-agent` ReAct pause trigger producer
- 关联：[ADR 0259](0259-memory-runtime-committed-event-consumer.md)、[ADR 0264](0264-memory-trigger-interval-producer.md)

## 决策

把 Ask、Confirm、TurnEnd、Budget、External 五类暂停路径统一为
`LoopHooks::on_pause -> Option<MemoryTriggerPayload>`。生产 hook 只生成
`trigger_kind=pause`、`bypass_throttle=true` 的 typed intent；Noop hook 不产生
intent。暂停原因使用稳定的 `ask`、`confirm`、`turn_end`、`budget`、`external`
字符串，作为现有 wire payload 的兼容字段。

暂停入口显式携带 `PauseReason`，不再通过当前仍挂起的交互事后推断原因；这避免同批
Ask+Confirm 首次 Confirm 暂停被错误标记为 Ask。`run_id` 从实际 ReAct run 贯穿到
暂停边界，`step_number` 使用调用方传入的 durable boundary step。

三个共同出口在 `ensure_event_boundary` 成功后调用 hook，并用独立取消 token 通过
`SessionStore` 追加 `memory_trigger`。追加失败或取消只记录日志，不改变已有 pause/
external-exit 结果；边界失败则不会产生 trigger。`InferCallback`、`DefaultHooks`
中的 worker 字段和 AgentLayer callback wiring 删除，MemoryRuntime 继续是唯一的
durable trigger consumer。

本切片不改变事实抽取、recall、rollback、summary extraction 或 session 状态转换；
事件仍按 append-only sequence 进入 MemoryRuntime。

## 验证

覆盖了五类 pause reason 的 typed intent、Ask 与 Budget 集成暂停事件、正确的
`run_id`/`step_number`、payload 校验、interval/pause 共用 producer，以及 agent 全量
测试、工作区测试、fmt 和 Clippy。
