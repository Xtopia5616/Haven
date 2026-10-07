# ADR 0731：MCP 单服务器重连命令标明目标实体

## 状态

已采纳并实施。

## 背景

MCP 管理命令的实体名与作用范围大多写在 command 名中：`list_mcp_servers` 返回服务器快照集合，`refresh_mcp_servers` reconcile 配置和 live client 的集合差异，`add_mcp_server`、`update_mcp_server`、`remove_mcp_server` 和 `toggle_mcp_server` 修改单个服务器。唯一例外 `reconnect_mcp` 也只操作一个 server，但命令名没有标出目标实体，无法和重连整个 MCP runtime 的动作区分。

唯一生产 renderer consumer 是 `ToolsView.handleReconnect`：它传入 `McpServerSnapshot.name`，成功后通知并重新读取服务器快照；授权确认等待由已有 interaction queue 表达，确认完成后沿既有 `mcp:status_change` 更新状态。Rust handler 校验 server 仍已启用且有 live client，捕获 ConfigService version，再构造 `NativeMcpOperationArgs::McpReconnect`。App admin owner 对配置版本、目标和 live client 状态复核后才执行连接副作用； stale/断开目标与授权失败返回 command error，未发生持久数据变更。

`NativeMcpOperationArgs::McpReconnect` 及模型可见 operation key `haven.mcp.mcp_reconnect` 属于 Admin/tool capability 契约，由 Admin operation metadata 和 Agent/tool catalog 使用；它们并非 renderer Tauri command 名，owner 与消费者不同，本次不改。

## 决定

- Rust/Tauri command 改为 `reconnect_mcp_server`，UI wrapper 改为 `reconnectMcpServer`。
- UI request alias 按真实 command owner 改为 `ReconnectMcpServerRequest`；`name` wire 字段保留为 `McpServerConfig` / `McpServerSnapshot` 的规范 server identity 字段。
- 更新 command registry、bootstrap registration、generated contract、ToolsView、IPC/security 文档、IPC drift owner map、命名规范、路线图与输出目录。
- 删除旧 command 与 wrapper 名，不加兼容 alias。批量 reconcile 继续叫 `refresh_mcp_servers`。

## 替代方案

- 保留 `reconnect_mcp`：拒绝。当前名称未区分单个 server 与 MCP runtime/集合范围，且与同族 command 的实体命名不一致。
- 改名为 `reconnect_mcp_servers`：拒绝。handler 只接收一个 server name 并执行单目标复核，复数会误报作用范围。
- 一并重命名 Admin capability operation：拒绝。其正式 owner 是 Admin operation contract，受模型目录、授权 key 与不同调用链消费，不是 Tauri renderer command alias。

## 影响与验证

这是破坏性的 Tauri command 名称及 UI wrapper/request alias 变更。参数 `name`、响应 `()`、服务器配置、授权、连接/事件生命周期与失败语义不变；不影响数据库或配置持久结构，无需重置数据。旧 command 不保留。

验证：`cargo fmt --all -- --check`、`cargo check --workspace --locked`、`cargo clippy --workspace --locked -- -D warnings`、`cargo test --workspace --locked`、UI `check` / `test:run` / `build`、`scripts/check-ipc-contracts.ps1`（80 handlers）、`scripts/check-ipc-events.ps1`（35 channels）、`scripts/check-adr-index.ps1`（714 ADRs）与 `git diff --check` 均通过。

## 回滚

如回滚，必须同时恢复 Rust handler/注册、安全目录、generated contract、UI wrapper/调用方、IPC owner checker 与当前文档；不涉及持久数据回滚。
