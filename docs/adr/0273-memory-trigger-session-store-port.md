# ADR 0273：memory trigger producer 只依赖 SessionStore

- 状态：Accepted
- 日期：2026-09-24
- 范围：Agent ReAct memory trigger producer 与 Memory SessionStore
- 关联：[ADR 0259](0259-memory-runtime-committed-event-consumer.md)、[ADR 0265](0265-memory-trigger-pause-producer.md)、[ADR 0272](0272-usage-runtime-session-store-port.md)

## 背景

memory trigger 已经是 `session_events` 中的 durable producer，但 ReAct hook 仍把
`Arc<Database>` 和 `SessionStore` 一起传给 producer：Agent 侧先检查 session 是否存在，再
通过 store 追加事件。这样同一个事件边界的 blocking 调度和存在性检查仍暴露在 Agent。

## 决策

由 `SessionStore::append_if_session_exists` 负责可取消的 blocking 调度、session 存在性检查、
事件追加和 live event 广播。memory trigger producer 只接受 `SessionStore`，保留 payload 校验、
best-effort/nonfatal 语义和 synthetic session 的 no-op 行为。ReAct hook 不再传递
`Arc<Database>`。

不改变 `memory_trigger` event type、payload、run/step metadata、sequence、cancellation 或
MemoryRuntime replay 语义；数据库仍只在 Memory 内部被访问。

## 影响与验证

- 四个 ReAct trigger 入口不再直接传 raw Database；
- 事件追加与 session 存在性检查共享 SessionStore 端口；
- 已验证已存在会话、取消、数据库触发器失败和 synthetic session 四种路径；
- 通过 workspace check、Agent memory-trigger focused tests、严格 Clippy 和完整 workspace test。

## 回滚

可回退 producer 参数和 SessionStore 端口，恢复调用方传入 `Arc<Database>`；无需数据库或 IPC
迁移。
