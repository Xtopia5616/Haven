# ADR 0293：terminal ingress ghost message 清理通过 SessionStore

- 状态：Accepted
- 日期：2026-09-24
- 范围：AgentLayer terminal-session ingress fallback 中已持久化用户消息的 best-effort 清理
- 关联：[ADR 0256](0256-session-message-session-store-port.md)、[ADR 0292](0292-agent-layer-session-writes-through-store.md)

## 背景

`AgentLayer::process_input_with_attachments` 先持久化 ingress 用户消息。当补充输入无法进入内存队列、session 重载后确认已是 terminal 时，它必须删除这条消息，避免历史中留下 ghost bubble。此前 Agent 在此 fallback 内直接用 `Database::run_blocking` 调度 `delete_message_by_id`，重复暴露了 SQLite blocking-pool 边界。

AgentLayer 的其他职责仍使用 raw `Database`。本切片仅迁移此项 terminal ghost-message cleanup，不改动 lifecycle 决策或其他数据库路径。

## 决策

1. 为 `SessionStore` 增加具体异步 `delete_message_by_id(session_id, message_id)` 端口。该端口只复制 ID、在 blocking pool 调用既有 `Database::delete_message_by_id` 并原样返回结果/错误；消息删除和 cache invalidation 仍由原 Database 方法负责。
2. AgentLayer 仅在用户消息已持久化，且 fresh session status 为 terminal 时调用该端口。保持 `session_id`、`message_id` 及 warning 中的错误文本。
3. 删除失败只写现有 warning；仍继续 emit `SessionUpdated`、移除 session，并返回 `ProcessResult::Supplemented(None)`。删除成功路径和其他 ingress 路由不变。
4. AgentLayer 保留 raw `Database` 供其他职责使用。不把 terminal/lifecycle policy 下沉到 Memory，不添加 generic trait/facade，也不扩展到 rollback、partial store 或 memory runtime。

直接使用 `Database::run_blocking` 让 Agent 重复持有存储调度机制；把 terminal 判断或降级策略移进 Store 则会扩大 Memory 的业务职责。窄异步端口能复用已有持久化实现并保留 Agent 的所有权边界。

## 影响与验证

- 无 schema、IPC 或用户数据契约变化，无需重置数据库。
- Memory 测试通过 `SessionStore` 验证按 ID 删除消息；Agent 集成回归测试验证 terminal 路径删除成功，以及 SQLite trigger 阻止删除时消息保留、terminal 状态事件、actor 移除和 `Supplemented(None)`。失败 warning 调用及原始格式串保持不变，并通过代码审查确认。
- 验收：`cargo fmt --all -- --check`、`cargo test --locked -p haven-memory`、`cargo test --locked -p haven-agent`、`cargo clippy --locked -p haven-memory -p haven-agent -- -D warnings`。条件允许时运行 workspace check/clippy/test。

## 回滚

将 ingress terminal fallback 恢复为 Agent 内的 `Database::run_blocking` 调用，并移除 `SessionStore::delete_message_by_id`、对应测试、本 ADR、索引和路线图记录。无需迁移或重置数据库。
