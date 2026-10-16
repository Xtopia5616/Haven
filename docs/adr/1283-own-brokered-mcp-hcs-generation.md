# ADR 1283：拥有 brokered MCP HCS generation 生命周期

## 状态

已实现私有 HCS MCP guest owner 并通过 HCS mock tests。尚未接入 broker `McpSessionOpen` handler、session pipe relay、授权撤销或真实 HCS；`WorkspaceWrite` 下 stdio MCP 继续 fail closed。

## 背景

ADR 1279–1282 已固定 session wire contract、独立认证管道、有界 stdin worker 和统一 stdio adapters，但尚无 owner 以一个生命周期管理 quota-bound compute system、固定 managed Node process、双向 stdio 和 stop proof。若由 transport 直接操作这些句柄，容易在客户端断连或撤权后释放 writable layer 时，guest 仍然存活。

## 决定

- 增加私有 `HcsMcpGuestSessionOwner`，创建并启动一个由调用方提供的 `WindowsContainerSandboxConfig` 对应的 HCS compute system；owner 不映射 `ExecutionWorkspace` 或 Skill input。
- compute system 创建后可以暂不启动 guest process，给 service 一个先接受并认证 session-pipe attach 的窗口。`launch_package` 只接受通过启动 quota probe 得到的 opaque `McpNodePackageProof`，并通过固定 `from_mcp_package` builder 创建 Node process；没有 command、argv、environment、cwd 或 host path 参数。
- process 创建后由 `HcsGuestProcessStdioOwner` 独占，提供 16 KiB/64 KiB 输入限制、EOF、bounded stdout/stderr drain 和 exit polling。该 owner 只搬运 bytes，不做 MCP JSON-RPC parsing；`haven-mcp` 是 framing 和取消唯一 owner。
- `stop` 先 abort/收束 process stdin writer 并释放 process handles，再要求 HCS 终止 compute system；只有 HCS 返回确认后才产生 `HcsStoppedComputeSystem`。若 process/stdin 或系统停止任一步失败，不向调用方返回 proof；attempt 必须按现有 quarantine 语义保留。
- 未显式 `stop` 时 Drop 只做 best effort cleanup，不能产生 stop proof。broker service 仍须把其 attempt owner 标记为 active，并在不确定停止时 quarantine；不能把 Drop 当成可安全删 scratch 的证据。
- 该 owner 在当前调用线程执行，不把 `NativeHcsApi`、attestation 或 workspace capability 发给 worker。service 需在调用前验证精确 quota、runtime/package proof 与当前授权；本切片不授权 MCP，也不改变控制面并发策略。

## 替代方案

- 在 HCS process owner 外单独保留 compute-system raw HANDLE：拒绝，会失去唯一 stop/close owner 及 scratch 生命周期关联。
- 从 `McpSessionOpen` 直接传 HCS JSON/Node command：拒绝，wire 只允许受管 package ID，host 重新从 pinned catalog 构造 process。
- 在 HCS guest 或 broker 解析/重写 JSON-RPC：拒绝，stdio 必须保持字节透传。

## 影响与回滚

新增 Windows 平台内部 HCS lifecycle owner 与测试；没有持久化、配置、IPC/UI 或数据库变化，无需重置。回滚时移除 owner 和相关 ADR/路线图记录；broker MCP 仍按现状 fail closed。

## 验证

- HCS mock tests 覆盖固定 package 参数、无 workspace mount、双流字节读取、stdin EOF、process exit、system stop proof，以及 Drop 时先终止 process 再终止 system。
- 本切片完成后运行平台 locked tests、strict Clippy、格式和 ADR 索引检查。
- 未启动真实 broker service 或 HCS MCP guest；Quota/HCS acceptance 按用户要求暂不执行。
