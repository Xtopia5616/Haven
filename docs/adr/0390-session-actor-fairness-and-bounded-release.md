# ADR 0390：SessionActor 饱和公平性与有界 run 释放

- 状态：已采纳并实现（2026-09-28）
- 关联：[ADR 0214](0214-react-run-inside-session-actor.md)、[ADR 0382](0382-session-state-owns-react-run.md)、[ADR 0360](0360-core-pipeline-performance-baselines.md)

## 背景

SessionActor 的容量为 128 的 mailbox 与 active run future 曾通过 `select! { biased; ... }` 调度。持续就绪的 mailbox 分支可能压住 run future。同步 direct-run guard drop 在 mailbox 满时还会为 `ReleaseRun` 发送各自创建一个等待 task，使释放信号的排队量随压力增长。

现有 provider-await 测试只证明 run 挂起期间能够接收命令，没有覆盖 mailbox 持续饱和时 run 推进、排队 sender 和 cancel 的活性，也没有规定 run release 的溢出边界。

## 决定

1. SessionActor 的 mailbox、direct-run release 信号、Run future 与 ReAct future 使用 Tokio `select!` 默认公平调度；不让持续就绪的 mailbox 固定优先。该约束是避免分支饥饿的运行时策略，不提供固定延迟 SLA。
2. 主 mailbox 保持容量 128。通过异步 handle API 发送的命令在满载时等待容量，actor 在持续负载下继续处理它们；已有同步 best-effort 命令保留各自语义。
3. 同步 `release_run_now` 使用独立的容量为 1 的 mailbox。重复 release 是幂等操作；队列已满时合并到现有信号，不 spawn 等待发送 task。actor 在处理下一条普通命令前先应用已排队的 release，保证后续 `FinishRun` / 新 run 不越过更早的 release。actor 关闭时忽略 closed-channel 结果。
4. 饱和负载验收必须同时覆盖：持续填充主 mailbox 时 Run future 继续推进；等待发送的 Snapshot roundtrip 能完成；Cancel 得到 actor 应答且 run 退出；mailbox 满时大量同步 release 最多占据一个信号并能清除 run 状态。测试超时只作为 liveness 保护，不是生产延迟阈值。

## 验证与实现边界

回归位于 `crates/agent/src/session/actor.rs` 的 `queue_tests`，分别覆盖饱和负载下 run/sender/cancel 进展和单槽 release 合并。`actor.rs` 已超过开发标准建议的 800 行；本切片继续将 select 调度和 `SessionState` 转换放在 actor owner 中，避免为单一局部协议新造跨模块边界。测试和 mailbox 定义继续留在同一私有模块。

```text
cargo test --locked -p haven-agent session::actor::queue_tests --lib
```

没有数据库、事件、IPC 或用户数据变化；普通 mailbox 命令顺序和 run lifecycle API 不变。

## 回滚

回退 actor 调度与 one-slot release mailbox 的实现、测试和本文即可；无需数据重置。
