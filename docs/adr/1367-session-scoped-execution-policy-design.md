# ADR 1367：会话级执行上下文与权限分层提案

## 状态

部分实施 — 2026-10-22 起按 [ADR 1373](1373-session-scoped-execution-workspace.md) 与 [ADR 1374](1374-session-execution-mode-and-resource-contracts.md) 分片落地。本文保留总体目标和未完成方案；尚未实施部分不替代当前架构、安全行为或既有 ADR。

## 背景

当前全局 `SecurityConfig.execution_root`、含 Plan 的 `PermissionMode`、含 Ask 的 `NetworkPolicy`、文件写范围 `SandboxMode` 和混合调用通道的 `McpToolExposure` 在不同时期引入。现有 proof/目标绑定/执行前重检已提供重要安全保障，但用户选择、上下文归属与实际隔离能力仍缺少清楚的一致层级。

## 提议

采用[完整设计](../execution-context-redesign.md)：

1. 会话持久拥有可选工作目录与执行策略绑定；一次 run 捕获同一上下文。工具后台任务和 Schedule 保存自己的绑定，不跟随 UI 当前目录。
2. Workspace 负责位置，Policy 负责资源和操作授权/审批，Execution Environment 负责落实边界，Code Mode/原生调用/Workflow 属于编排入口。
3. 保留 AuthorizationEngine 作为唯一授权决策 owner 和现有 proof，补充 typed 资源声明、上下文绑定与明确的边界扩展。Analyze 是任务约束；不弹窗意味着必要确认时拒绝，不意味着放行。
4. 进程隔离由实际 backend 能力和 spawn 证明；现有 MXC 工作可独立推进，接入时不引入宽权限自动降级。桌面/远程操作不伪装成工作区文件操作。
5. 常规 UI 收敛为工作目录和执行方式；工具可用性、目录展示与 Code Mode 调用支持分开表达。保留 ADR 1321 的 nested confirmation、journal 和未知结果停止语义。

这不是增加一套 profile/manager 框架。初期不增加 Workspace 实体、自定义继承 profile 或新的任务生命周期。

## 替代方案

- 只改标签/移动设置：不能消除全局工作根对会话和后台任务的影响，也不能拆开网络限制与询问语义。
- 统一成一个权限等级：无法同时准确表达本地文件、OS 进程、桌面操作和远端能力。
- 为 Code Mode 单独设计授权：产生第二 owner，并使相同能力因调用通道得到不同安全语义。
- 直接采用另一个 Agent 的全部配置：不符合 Haven 的 Windows 助手定位，且当前产品能力和平台限制不同。

## 影响、实施与重置

本 ADR 的会话工作绑定、执行预设及核心资源合同分别由 ADR 1373、1374 实施；尚余进程路径盘点、MCP exposure 拆分、完整资源类型与范围扩展。已实施切片未变更数据库 schema，按各实施 ADR 记载配置兼容和数据影响；后续切片仍须记录旧宽泛 allow、MCP 调用限制及 schema 的保守转换/重置要求。

MXC Shell 隔离另由 ADR 1368 记录。主流实现的官方来源及其适用限制见完整设计第三节。

## 验证与回滚

各实现切片的代码、测试与限制见对应 ADR。总体设计的剩余部分必须覆盖完整设计第十节的正向、拒绝、竞态、撤权、重启与未知副作用矩阵。

撤回提案只需移除设计、该 ADR 和候选/索引链接；不影响运行数据或现有实现。
