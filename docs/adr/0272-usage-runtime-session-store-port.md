# ADR 0272：UsageRuntime 只依赖 SessionStore

- 状态：Accepted
- 日期：2026-09-24
- 范围：Agent usage runtime 与 Memory SessionStore
- 关联：[ADR 0223](0223-usage-runtime-ownership.md)、[ADR 0255](0255-transcript-batch-session-store-port.md)、[ADR 0270](0270-typed-cache-accounting.md)、[ADR 0271](0271-typed-llm-call-kind.md)

## 背景

`UsageRuntime` 已经拥有 Agent 侧的用量 FIFO、累计 tracker 和 rollback epoch，但它仍同时持有
`Arc<Database>` 与 `SessionStore`。Agent 因此仍需知道如何把 SessionStore 操作调度到 SQLite
blocking pool，读取、追加和补偿写入在不同闭包中重复表达同一存储边界。

## 决策

由 `SessionStore` 暴露三个与 usage 生命周期对应的可取消异步端口：读取 materialized
`SessionUsage`、批量追加 `LlmCallUsageInput`、追加失败 epoch 补偿用的 discard event。端口内部
负责 blocking pool、SQLite interrupt cancellation、事件广播和 projection 细节。

`UsageRuntime` 只保留 `SessionStore`，并继续独立拥有 Agent usage 的 FIFO、累计状态和 epoch
判定。epoch 判定仍在 Agent 内部；确认失效后，discard 通过 SessionStore 端口执行且不被原始
请求取消打断，以保持 X12 usage event 的补偿语义。数据库读模型、事件顺序、IPC shape 和
rollback 语义不变。

## 影响与验证

- Agent usage runtime 不再传播 `Arc<Database>`；
- tool/media usage 批量追加与 Agent usage seed/persist 共用 SessionStore 调度边界；
- 用量 FIFO、取消、并发累计、epoch invalidation 和事件投影测试继续覆盖；
- 通过 workspace check、Agent usage focused tests、严格 Clippy 和完整 workspace test。

## 回滚

可回退 `UsageRuntime` 的 SessionStore-only 构造器和三个端口，恢复由 Agent 直接调度
`Database` 的实现；无需数据库或 IPC 迁移。
