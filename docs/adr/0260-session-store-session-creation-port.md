# ADR 0260：SessionStore 会话创建端口

- 状态：Accepted
- 日期：2026-09-24
- 范围：`haven-agent` 会话创建与 `haven-memory::SessionStore`
- 关联：[ADR 0249](0249-session-store-session-record-reads.md)、[ADR 0256](0256-session-message-session-store-port.md)

## 决策

新增 `SessionStore::create_session`，由 `SessionStore` 负责把会话记录创建调度到 blocking pool；`SessionSupervisor` 和 `AgentLayer` 不再直接通过 `Database::create_session` 创建生产会话。

该端口只负责 durable session row。生命周期闸门、首条用户消息的写入顺序、actor 安装、pending 入队和 dispatcher 唤醒仍由 Agent 层拥有；首条消息失败时的补偿删除也保持原有路径。本切片不追加 session event，不改变 schema、X12 transcript 约束或 actor 单写者模型。

## 验证

- `SessionStore` 测试确认创建结果可读回、状态为 `Pending`，且创建本身不追加事件。
- Agent 的既有创建测试继续覆盖普通创建与带 summary 创建。
- 通过 `cargo fmt --all -- --check`、Memory/Agent 定向测试与 `git diff --check`。
