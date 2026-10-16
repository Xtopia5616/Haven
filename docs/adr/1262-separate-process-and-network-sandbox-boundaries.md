# ADR 1262：在 sandbox policy 中拆分本地进程与网络边界

## 状态

已采纳并实施。

## 背景

`WorkspaceWrite` 原先用 `NetworkAccess::Opaque` 同时表示“目的地不可检查”和“可能启动能访问宿主文件系统的本地进程”。这让 stdio MCP、Skill 与 HTTP MCP 共享同一 sandbox 拒绝条件。HTTP MCP 由 Haven 的进程内 HTTP transport 调用配置服务器，不会启动 stdio 子进程；但它仍是 opaque network capability，必须继续受 Ask/Restricted/Deny 和现有披露确认控制。

此外，Windows Shell 的 HCS 例外以前由授权器按工具名 `shell` 判断。工具名称不是执行隔离证明，不能作为绕过本地进程 sandbox 的权限。

## 决定

- `OperationPolicy` 增加 typed `ExecutionBoundary`：`InProcess`、`HostProcess`、`WorkspaceIsolated`。它与 `NetworkAccess` 分开表达，并随授权 proof 中完整的 `OperationPolicy` 一起绑定和复核。
- `WorkspaceWrite` 只在 `HostProcess` 上硬拒绝。网络边界独立生效：HTTP MCP 即使 `InProcess` 仍保持 `NetworkAccess::Opaque`；`Restricted` 与 `Deny` 拒绝，`Ask` 仍走现有确认和 grant 规则。永久/会话授权及 proof 重验仍经过相同边界。
- MCP adapters 从可信服务器配置的 transport 生成执行边界：stdio 为 `HostProcess`，HTTP 为 `InProcess`。原生、Deferred 注册、目录刷新、Workflow 与 CodeMode 的实际子调用解析均传递该配置。默认 adapter 构造保持 stdio/`HostProcess`，避免缺少 transport 证据时放宽。
- Skill 与可启动或连接任意 MCP transport 的 loader/Admin 路径采用保守的 `HostProcess`。`load_mcp` 目前仍可能连接 stdio，因此在 WorkspaceWrite 下拒绝；后续若要放开 Deferred HTTP loader，必须让授权前的 server config 解析成为 typed、proof-bound policy 输入，而不能相信模型参数声明的 transport。
- MCP stdio 的进程启动 permit 要求 `FullAccess + NetworkPolicy::Open`；切入 `WorkspaceWrite` 会撤销已有代次并阻止新进程在 OS spawn 前启动。HTTP MCP 不需要该子进程 permit，仍走每次调用授权。
- 只有 Windows Shell owner 显式声明 `WorkspaceIsolated`；非 Windows Shell 与仅凭名称合成的未知工具保留 `HostProcess`。Windows WorkspaceWrite 执行仍必须使用 quota-attested HCS；不可用时继续 fail closed。

## 替代方案

- 继续用 `NetworkAccess::Opaque` 拒绝所有 MCP：拒绝，会把远程 HTTP transport 错当成本地任意进程，并阻断已授权的配置目标调用。
- 放开所有 MCP 或按工具名称判断：拒绝，stdio 子进程仍能访问宿主文件系统，且名称不能证明 OS 隔离。
- 仅依赖用户确认限制子进程：拒绝，确认不构成文件系统或网络的 OS sandbox。

## 影响与迁移

WorkspaceWrite + Ask/Open 下，transport 已知的 HTTP MCP operation 可以继续走现有授权与目标绑定；HTTP 网络仍不满足 Restricted/Deny。stdio MCP 在 WorkspaceWrite 下不会 spawn，已有启动 permit 随 sandbox 边界变化失效；Skill、`load_mcp` 及其它 `HostProcess` 能力继续 fail closed。Windows Shell 的隔离许可来自 Shell policy 本身，不再由任意 `shell` 名称触发。`ExecutionBoundary` 是 Tools 内部执行策略字段，不改变 renderer DTO、设置持久格式或数据库 schema。

## 验证

- adapter 测试覆盖 HTTP/stdio transport 对应不同执行边界，同时确认 HTTP 的 `NetworkAccess` 仍是 `Opaque`。
- 授权测试覆盖 WorkspaceWrite 对 HostProcess 的拒绝、stdio process-start permit 的撤销、HTTP MCP 与网络策略的独立判断，以及执行边界变化会使旧 proof 失效。
- Shell policy 测试确认 HCS 边界由 Shell owner 显式声明，单凭 `shell` 名称仍是 HostProcess。
- 运行 Tools 测试、workspace 检查与 Clippy、格式和架构/文档门禁；不运行真实 HCS 验收。
