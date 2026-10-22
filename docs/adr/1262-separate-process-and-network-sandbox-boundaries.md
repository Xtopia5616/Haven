# ADR 1262：区分宿主进程与网络授权

> WorkspaceWrite Shell 后续通过 MXC ProcessContainer 获得受限文件/网络 capability，见 [ADR 1368](1368-mxc-processcontainer-for-workspace-shell.md)；本文对 Skill 与 MCP stdio 的拒绝仍有效。

## 状态

已采纳并实施。

## 背景

`NetworkAccess::Opaque` 表示 Haven 无法按目标地址检查网络请求；它不说明操作是否启动宿主子进程。stdio MCP 和 Skill 会启动宿主进程，HTTP MCP 使用进程内 transport。两者需要独立授权。

## 决定

- `OperationPolicy` 使用 typed `ExecutionBoundary`：`InProcess` 与 `HostProcess`。它与 `NetworkAccess` 分开表达，并随授权 proof 中完整的 `OperationPolicy` 一起绑定和复核。
- `WorkspaceWrite` 只在 `HostProcess` 上硬拒绝。网络边界独立生效：HTTP MCP 即使 `InProcess` 仍保持 `NetworkAccess::Opaque`；`Restricted` 与 `Deny` 拒绝，`Ask` 仍走现有确认和 grant 规则。永久/会话授权及 proof 重验仍经过相同边界。
- MCP adapters 从可信服务器配置的 transport 生成执行边界：stdio 为 `HostProcess`，HTTP 为 `InProcess`。原生、Deferred 注册、目录刷新、Workflow 与 CodeMode 的实际子调用解析均传递该配置。默认 adapter 构造保持 stdio/`HostProcess`，避免缺少 transport 证据时放宽。
- Skill 与可启动或连接任意 MCP transport 的 loader/Admin 路径采用保守的 `HostProcess`。`load_mcp` 目前仍可能连接 stdio，因此在 WorkspaceWrite 下拒绝；后续若要放开 Deferred HTTP loader，必须让授权前的 server config 解析成为 typed、proof-bound policy 输入，而不能相信模型参数声明的 transport。
- MCP stdio 的进程启动 permit 要求 `FullAccess + NetworkPolicy::Open`；切入 `WorkspaceWrite` 会撤销已有代次并阻止新进程在 OS spawn 前启动。HTTP MCP 不需要该子进程 permit，仍走每次调用授权。
- 所有 Shell、Skill 和 MCP stdio 子进程均为 `HostProcess`，在 `WorkspaceWrite` 下拒绝启动；不会因平台或工具名称而绕过此边界。

## 替代方案

- 继续用 `NetworkAccess::Opaque` 拒绝所有 MCP：拒绝，会把远程 HTTP transport 错当成本地任意进程，并阻断已授权的配置目标调用。
- 放开所有 MCP 或按工具名称判断：拒绝，stdio 子进程仍能访问宿主文件系统，且名称不能证明 OS 隔离。
- 仅依赖用户确认限制子进程：拒绝，确认不会限制宿主进程的文件或网络权限。

## 影响与迁移

`WorkspaceWrite` 下，HTTP MCP 仍按网络策略和目标绑定授权；stdio MCP、Skill 和 Shell 子进程不会启动。`FullAccess` 下获准的宿主进程使用当前 Windows 用户权限。`ExecutionBoundary` 是 Tools 内部执行策略字段，不改变 renderer DTO、设置持久格式或数据库 schema。

## 验证

- adapter 测试覆盖 HTTP/stdio transport 对应不同执行边界，同时确认 HTTP 的 `NetworkAccess` 仍是 `Opaque`。
- 授权测试覆盖 WorkspaceWrite 对 HostProcess 的拒绝、stdio process-start permit 的撤销、HTTP MCP 与网络策略的独立判断，以及执行边界变化会使旧 proof 失效。
- Shell policy 测试确认 Shell 使用 `HostProcess`，WorkspaceWrite 拒绝启动。
- 运行 Tools 测试、workspace 检查与 Clippy、格式和架构/文档门禁。
