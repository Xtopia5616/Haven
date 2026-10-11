# 1001：Agent 生命周期状态使用 Common 唯一入口

## 状态

已采纳并实施。

## 背景

ADR 0933 将 `SessionStatus`、`SessionStepStatus`、`SessionWaitingReason` 与 `ToolRunStatus` 的唯一导入路径规定为 `haven_common::lifecycle`。复核 Agent API 时发现，Agent session 模块仍将 `SessionStatus` 和 `SessionWaitingReason` 公开重导出，Agent crate root 又继续暴露这两个 Common 所有的类型。App 生产代码已使用 Common 路径，只有两处 App 测试依赖 Agent 别名。

## 决定

- Agent session 实现仅从 `haven_common::lifecycle` 内部导入 `SessionStatus` 与 `SessionWaitingReason`，不再公开重导出。
- Agent crate root 移除这两个 Common 生命周期类型的别名。
- App 测试从 `haven_common::lifecycle` 导入 `SessionStatus`，使跨 crate 使用路径表达类型 owner。
- 保持生命周期类型、序列化、持久化与运行行为不变。

## 影响

这项未发布 workspace API 收窄兑现 ADR 0933 的唯一入口约定，不改变数据库、事件、IPC、配置或用户可见行为，无需重置数据。仓库内没有生产消费者依赖 Agent aliases。

## 验证

- 静态搜索确认 Agent/App 不再引用 `haven_agent::SessionStatus` 或 `haven_agent::SessionWaitingReason`。
- `cargo fmt --all -- --check`
- `cargo check --workspace --locked`
- `cargo test --workspace --locked`
- `cargo clippy --workspace --locked -- -D warnings`
- ADR 索引检查。

## 回滚

如确有跨 crate Agent-owned contract 消费需求，应先将状态类型迁至正确 owner；不得重新建立 Common lifecycle 类型的 Agent 别名。
