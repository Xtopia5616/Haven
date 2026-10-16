# ADR 1286：将受管 MCP stdio generation 接入容器 broker

## 状态

Windows 应用侧 launcher、目标 profile 授权和 broker session client 已接通；平台/MCP/Tools 相关测试通过。真实 HCS、guest 超额写入与 Windows 支持矩阵验收按用户要求暂缓，继续作为发布 Gate。

## 背景

ADR 1269–1285 已定义并实现受管 MCP package catalog、quota/runtime/package proof、broker HCS generation 与认证双管道协议。此前应用侧默认 MCP stdio launcher 仍在本机启动，managed profile 被拒绝，因此 `WorkspaceWrite` 下没有受管 MCP 执行路径。

## 决定

- MCP launcher 按已绑定的 `McpStdioExecution` 选择实现：`HostProcess` 使用本机 launcher；`ManagedPackage` 在 Windows 使用 broker session port，在非 Windows 明确失败。没有本机 managed-package 回退。
- MCP process-start authorization 接收 typed execution profile。`WorkspaceWrite` 拒绝 `HostProcess`，只允许受管 package；所有 stdio profile 仍要求 `NetworkPolicy::Open`。permit 绑定当前 process-boundary revision 和 workspace generation，并持有到 `open_session` 返回。attach 后取消时，`open_session` 等待匹配的 `McpSessionExited`；最多等待 60 秒，超时显式失败且不声称已获得停止证明。其它启动失败返回 broker 错误，由 service 保留其 quarantine/recovery 责任。
- open 控制握手可被调用取消打断。收到 broker-minted session ID 后，客户端先认证 session pipe，再重新检查取消，最后才发送 attach；因此 attach 前取消不会启动 guest。session pipe 使用独立 cancellation event，不随应用 launch token 断开。
- attach 后的取消经独立 control pipe 发送 Stop，并保留 session pipe 读取状态。客户端只把匹配的 `McpSessionExited` 当作 broker 停止并清理完成的确认；等待最多 60 秒，超时返回明确错误，不伪报停止成功。Stop 写入失败时按 250 ms 间隔重试；成功写入后不重复发送。
- MCP 侧仍使用有界 64 KiB Tokio duplex stream 和 16 KiB relay buffer；stdin 背压时只保留当前单帧，stdout/stderr 原样转发并保持异步 stdio 生命周期。`start_kill` 只发出一个 cancellation signal，由 session relay 发送 Stop 并继续读取到 broker Exited，避免两个并发 Stop。
- broker 请求只传 catalog package ID，不传用户命令、参数、环境变量或 workspace 路径。当前 managed MCP profile 不挂载 `ExecutionWorkspace`，项目文件依赖型 server 仍不可用于 `WorkspaceWrite`，直到另行完成 per-call workspace snapshot 与按 MCP tool step 绑定的输出审阅。
- 本次不改变持久配置、数据库或 wire protocol；继续使用 protocol v10。HCS quota attestation 是实际执行前置门禁，但启动 probe 的真实 Windows 容器验收仍未完成。

## 替代方案

- managed profile 回退到本机 `Command`：拒绝，会把配置声明的受管执行边界变成普通宿主进程执行。
- 把 launch cancellation 直接绑定 session pipe：拒绝，断开会隐藏 broker 的 Exited/stop proof，且可能在 guest 停止前结束启动流程。
- `start_kill` 与 session relay 并发发送 Stop：拒绝，增加重复控制请求并破坏单一停止/退出顺序。

## 影响与回滚

Windows 的受管 MCP profile 现在使用 broker session transport；`WorkspaceWrite` 下只有固定受管 package profile 可启动，任一授权、配置或 quota/package proof 缺失仍 fail closed。FullAccess 的 HostProcess profile 保留本机路径。没有数据库或持久配置变更，无需重置数据。回滚时恢复应用侧 profile-aware launcher 和 start authorization 的先前实现；broker/client protocol v10 必须保持成对，不与 v9 混用。

## 验证

- 启动取消状态机测试覆盖 Ready 边界取消、Stop 发送、等待 broker cleanup exit，以及正常 Ready 发布。
- `cargo test --locked -p haven-platform`、`cargo test --locked -p haven-mcp` 与 `cargo test --locked -p haven-tools mcp_stdio_process_authorization`。
- 本轮不运行真实 HCS、quota 超额写入或目标机验收；该限制来自用户明确要求，发布 Gate 保持开放。
