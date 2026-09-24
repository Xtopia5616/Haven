# ADR 0258：失败会话 action-step 清理通过 SessionStore

- 状态：已采纳（2026-09-24）
- 范围：`SessionSupervisor::fail_pending_action_steps` 与 `haven-memory::SessionStore`
- 关联：[ADR 0196](0196-session-actor-event-sourced-state.md)、[ADR 0256](0256-session-message-session-store-port.md)

## 背景

dispatcher 在 session 执行失败时，先把 session 状态更新为 `Error`，再将该 session
尚未结束的 action steps 标为 `unknown`，最后发布 `SessionError`。清理目前由
`SessionSupervisor` 直接通过 `Database::run_blocking` 调用 session-steps repository，
绕过已有的 SessionStore typed boundary。

## 决定

1. `SessionStore::fail_pending_action_steps` 提供明确的异步端口，并通过现有 blocking
   pool 调用 `Database::fail_pending_action_steps`；不复制 SQL，也不提供通用闭包接口。
2. `SessionSupervisor::fail_pending_action_steps` 只调用该 store 方法，并继续忽略持久化
   错误。dispatcher 的调用仍在 session Error 更新之后、`SessionError` 发布之前。
3. 保留底层操作的所有语义：仅影响目标 session 中 `pending`/`running` 的行，状态设为
   `unknown`，写入给定 observation 和完成时间；其他 session 与已完成行不变。
4. 此边界调整不改变 ActionService 的 action 生命周期或 MemoryRuntime 的事件消费职责。

## 替代方案

- 保留 Agent 对 Database 的直接调用：代码改动最少，但继续让 Agent 承担 blocking 调度和
  跨层存储入口。
- 暴露通用 blocking 闭包：可复用范围更大，但会让 Agent 任意执行 Memory 内部数据库逻辑，
  使 typed boundary 失去约束作用。

## 影响与验证

无 schema、持久化格式或通知行为变化。SessionStore 异步测试覆盖 pending/running 转为
`unknown`、observation 与完成时间写入、completed 行保持原状态/observation/完成时间、
以及 session 隔离；既有 repository 测试继续验证底层更新。验证命令：

- `cargo fmt --all -- --check`
- `cargo test --locked -p haven-memory fail_pending_action_steps`
- `cargo test --locked -p haven-agent`
- `cargo clippy --workspace --locked -- -D warnings`
- `git diff --cached --check`

## 回滚

回退本提交并恢复 Agent 的原 blocking 调用即可；数据库 schema 无变化，不需要数据重置。
