# ADR 0153：权限边界的操作契约、网络覆盖与进程约束

- 状态：accepted
- 日期：2026-09-14
- 范围：`haven_common`、`haven_tools`、`haven_mcp`、`haven_app-binary`、Settings UI
- supersedes：ADR 0152 中“子进程 OS 级隔离仍是后续工作”的未完成项

## 背景

上一轮权限重构已经把确认模式、文件沙箱和网络策略拆开，但复核发现仍有五类可绕过或误判：Plan 会被永久 allow 提前短路；网络策略只覆盖少数工具名；`workspace_write` 会被误解为操作系统沙箱；AutoEdit 把 UI/调度/进程效果混入编辑；只读属性没有区分敏感数据读取和普通本地读取。

## 决策

1. `OperationPolicy` 增加 typed 的 `OperationEffect`、`DataSensitivity` 和 `NetworkAccess`。所有默认 Tool、operation view 和 native/UI 入口都使用同一份契约。
2. Plan 的“禁止修改”检查位于 grant 匹配之前；永久授权只优化当前允许的确认流程，不改变能力边界。
3. AutoEdit 仅对 `files.write/edit/patch/create_dir/copy/move` 且契约明确为 `WorkspaceWrite` 的调用自动放行。
4. NetworkPolicy.Deny 阻断所有非 None 网络能力；Restricted 只允许 Haven 能解析、验证并固定的 Public 目的地，Opaque 子进程/适配器必须显式 Open。
5. HTTP/MCP 禁止自动重定向；HTTP 工具和 MCP HTTP transport 在解析后固定已验证地址。网络边界变化会拆除旧 MCP client，要求显式重新发现。
6. Windows 子进程在启动后加入 Job Object，设置 kill-on-close，取消或 Haven 退出时回收后代。Job Object 只提供进程树和生命周期约束，不能替代 AppContainer；因此 `workspace_write` 下 opaque 子进程直接 fail closed，只有 `FullAccess + Open` 允许启动。
7. 删除生产代码中的未类型化 `AuthorizationEngine::check` / `verify_receipt` 兼容入口；native 调用必须构造完整 `OperationPolicy`。

## 不变量与验证

- 所有授权结果都经过技术边界、Plan、deny、allow 和确认模式的固定顺序。
- 改变 security snapshot 会使 receipt 失效，并同步 MCP 的连接边界。
- 安全测试覆盖 Plan + Always allow、敏感读取、AutoEdit 负例、opaque 子进程、MCP 网络策略和操作契约元数据。
- Rust workspace、Clippy、fmt、UI 类型检查和 UI 单测必须通过。

## 后续

如果未来需要在 `workspace_write` 中重新开放 Shell/Skill/MCP，必须接入真正的 Windows AppContainer（或等效外部 sandbox），并为文件系统、网络、进程创建和 reparse point 添加端到端测试；不能仅凭 Job Object 或工作目录恢复该能力。
