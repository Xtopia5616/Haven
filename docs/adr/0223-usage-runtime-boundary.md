# ADR 0223：Agent usage runtime 脱离 SessionActor mailbox

- 状态：已采纳（2026-09-24）
- 范围：`haven-agent` 的 Agent LLM usage 累计、持久化与 epoch 失效
- 关联：[ADR 0214](0214-react-run-inside-session-actor.md)

## 背景

Agent usage 的 `seed → 累计 → append_usage → epoch 检查` 原先在
`SessionActor` mailbox 中执行。这样虽然提供了同一 session 的串行边界，但
会让 actor 在等待数据库 blocking 操作时承担并不属于会话队列的持久化工作，
并保留 `RecordUsage`、`ResetUsage`、`InvalidateUsage` 三组 mailbox 形态。

## 决定

1. `UsageRuntime` 由 `ReActEngine` 持有，拥有 Agent usage 的 DTO、累计 tracker、
   per-session FIFO 和持久化流程。
2. 每个 session 使用独立的 async operation gate，覆盖冷启动 seed、累计、
   epoch 捕获、SQLite blocking 写入以及 reset/invalidate，保留原 mailbox 的
   顺序语义；`UsageTracker` 内部的同步锁不承担跨 await 串行化。
3. `SessionStore::append_usage`、`discard_usage`、`call_kind=agent`、取消行为、
   错误处理和成功后才发布 UI usage 事件保持不变。tool/media usage 不经过该
   runtime，继续走原有路径。
4. runtime 的 per-session state 不在本切片中删除或重建。这样 detached blocking
   worker 仍可观察同一个 epoch map，不会因 session 状态重建产生 ABA；后续若要
   回收，必须先建立可证明的 in-flight worker 生命周期协议。
5. 删除 actor 中对应的 usage 字段、命令、handle 方法和实现函数；不改变数据库
   schema、事件格式、wire API 或累计算法。

## 未采用方案

- 只把 `UsageTracker` 字段移到 `ReActEngine`：无法覆盖 seed 与 blocking 写入的
  跨 await 串行化。
- 继续让 actor 等待 usage 写入：会话 mailbox 继续被非会话职责阻塞。
- reset 时直接删除并重建 session runtime：在取消后的 blocking worker 仍可能运行时
  产生旧 epoch 与新状态的 ABA 风险。

## 验证

除已有 Agent、memory usage 事务和 rollback 测试外，runtime 测试覆盖同一 session
并发记录不丢累计，以及 reset/invalidate 与 record 的 FIFO 顺序。实现不改变 durable
usage 事务，后续应补可控的 truncate 与 detached worker 竞态测试后再设计状态回收。

## 回滚

回退本 ADR 对应提交并恢复 actor usage mailbox 路径即可；没有数据库或事件格式迁移。
