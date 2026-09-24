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

Phase 7.1 第三个切片在该核心上增加 `MemoryRuntime::run_until_cancelled`。启动时先订阅同一 `SessionStore` 的 live broadcast，再固定启动 session ID 集合；集合内只有 cursor key 缺失的旧 session 才以 `latest_sequence` 建立 baseline，不回放其历史 transcript。显式 cursor `0` 保持有效，不会被 baseline 覆盖。durable fact outbox 通过 `MemoryWorker` 的窄恢复入口启动；随后，在返回 live receiver 前，对当前可见 sessions 按已有 cursor 有界回放。启动 snapshot 之后创建且没有 cursor 的 session 从 sequence `0` 回放；可能与 live receiver 重叠的事件由 `process_event` 的 cursor 幂等处理。

live event 按 session 串行处理。sequence gap 从 durable `replay_page` 分页补齐（单页上限 256），再重试当前 event；cursor 覆盖范围内的 live/replay overlap 仍由 `process_event` 幂等跳过。broadcast lag 后枚举可见 session 并逐个分页恢复，不把完整 event transcript 载入内存。关闭或取消退出；错误写日志并采用可取消退避重试，取消不做 cursor checkpoint 或清除 durable outbox marker。

本切片仍不改变 memory trigger payload、ReAct hooks、dispatcher 启动顺序或 AgentLayer 装配。旧 `MemoryWorker::enqueue_infer` 的 best-effort 行为保留，只有 Runtime 使用严格可 await 的 durable enqueue 入口。

## 验证

覆盖普通事件、合法 interval/pause trigger、重复/升级、坏 payload、outbox/checkpoint 故障、sequence gap、跨 session 隔离和 cancellation-after-enqueue；另覆盖启动 baseline、缺失与显式零游标区分、outbox restore 调用、已有 cursor 后 trigger 的启动回放、分页 gap recovery、live/replay overlap 和 session recovery。验证 Memory/Agent 两 crate 测试、两 crate clippy、fmt 与 staged diff。
