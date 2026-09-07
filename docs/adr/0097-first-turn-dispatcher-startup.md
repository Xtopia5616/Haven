# ADR 0097：首次对话不等待工具目录启动

## 背景

桌面启动后的 MCP 连接、Skills 扫描和工具目录重建属于 deferred bootstrap。
此前 SessionExecutor dispatcher 只有在这些工作完成（或 10 秒超时）后才启动，
导致用户在窗口出现后立即发送的第一条消息也要等待 dispatcher。

同时，应用重启后留在 `pending` 的会话必须在工具目录完成后恢复，否则恢复回合
可能在空 MCP/Skills 目录上构造模型请求。

## 决定

将 dispatcher 启动与 durable pending-session recovery 分开：

1. 事件总线安装后立即启动 dispatcher，允许新建会话进入 ReAct。
2. 冷启动期间不自动加载旧的 pending 会话。
3. MCP/Skills 目录完成后显式调用 `recover_pending_sessions`，再把旧会话加入队列。
4. 保留 `AgentLayer::start` 和 `SessionExecutor::start_dispatcher` 的完整恢复语义，
   仅桌面 cold-start 使用 deferred recovery 入口。

## 影响

- 启动后第一条对话不再被工具目录扫描阻塞，收益通常是数百毫秒到数秒。
- 新会话在目录仍在加载时只能看到当时已注册的内置工具；当前回合的工具投影仍按
  ReAct 的 freeze-per-run 规则保持一致，后续回合会看到更新后的目录。
- 旧 pending 会话继续等待目录就绪后恢复，避免改变恢复时的工具可见性。
- 不涉及数据库、快照、IPC 或 ID 格式变更，无需数据重置。

## 替代方案

- 继续让所有会话等待目录：保持实现简单，但保留首轮延迟。
- 直接提前恢复旧 pending 会话：首轮更快，但可能在空目录上恢复并改变工具行为，拒绝。

## 验证

- `cargo check --workspace --locked`
- `cargo test --locked -p haven-agent dispatcher_respects_max_concurrent`
- `cargo test --locked -p haven-agent dispatcher_can_defer_pending_recovery_until_catalog_ready`

## 回滚

删除 desktop 对 `start_without_pending_recovery` / `recover_pending_sessions` 的调用，
恢复在 bootstrap catalog 完成后调用 `AgentLayer::start`；代码入口保留的完整恢复语义
不需要数据迁移。
