# ADR 0280：fresh-run conversation window 通过 SessionStore 读取

- 状态：Accepted
- 日期：2026-09-24
- 范围：Agent fresh-run prompt 的最近消息窗口读取
- 关联：[ADR 0249](0249-session-store-session-record-reads.md)、[ADR 0277](0277-context-source-session-title-port.md)、[ADR 0279](0279-app-history-session-store-ports.md)

## 背景

`AgentLayer::load_conversation_history` 只为 fresh-run prompt 读取最近的会话消息，
但直接从 `Arc<Database>` 调用 `run_blocking`。该读取路径不需要完整 `Message`，而且
属于 Memory 已有的 session 持久化边界。Resume 仍由 durable event stream 恢复；同一
方法之外的完整消息与附件读取承担不同用途。

## 决策

`SessionStore` 提供异步 `conversation_window(session_id, limit)` 端口，内部复用现有
`get_session_messages_limit` 查询，并只返回 `role/content` typed DTO。Agent 保留自己
定义的 `ConversationMessage`，在调用边界映射该 DTO，并继续包装为原有
`failed to load conversation history: ...` 错误文本。

现有 SQL 的消息类型过滤、最近 `limit` 条选择和时间正序结果保持不变。端口只用于
fresh-run prompt 的 Additional context；事件流恢复、fresh-run/resume 分界及完整附件
读取路径不变。SQLite 查询仍由 Tokio blocking pool 执行，丢弃调用方 future 不会停止
已启动的 blocking closure。

## 影响与验证

- Agent 不再直接调度该最近消息窗口查询，也不需要在此路径持有 raw `Database`；
- SessionStore 测试覆盖 limit、正序及无消息会话；
- 验收：`cargo fmt --all -- --check`、haven-memory conversation-window focused test、
  haven-agent resume focused tests、workspace check 和严格 Clippy。

## 回滚

可恢复 Agent 中的原有 `run_blocking` 查询并删除 `SessionStore::conversation_window`、
其 DTO 和对应测试；不涉及 schema、IPC 或数据重置。
