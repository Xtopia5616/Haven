# ADR 1190：明确 SandboxMode 的授权范围

## 状态

已采纳并实施。

## 背景

`SandboxMode` 是 Haven 授权网关对工具操作执行的文件/副作用边界，不会给子进程创建 Windows 限制令牌或 OS 级文件/网络隔离。设置页“文件沙箱”和类型注释容易让人把执行工作区、授权范围与 Windows OS sandbox 混为一谈；类型注释还曾暗示 `WorkspaceWrite + Ask` 可通过确认运行 opaque 子进程，与实际 fail-closed 逻辑不符。

## 决定

- 保留当前配置与 wire enum：`read_only`、`workspace_write`、`full_access`。本 ADR 澄清其作用范围，不更改运行时策略或持久化值。
- `ReadOnly` 表示 Haven 授权网关拒绝修改和外部副作用操作；这不会限制未经 Haven 执行网关启动的其它进程。
- `WorkspaceWrite` 将 Haven 管理的带路径写入限定到配置 roots；由于 Windows Job Object 只约束进程树/生命周期，不能限定子进程的文件或网络能力，Shell、Skill 与 opaque MCP capability 在此模式下一律拒绝，不受 `Ask` 或 `Open` 覆盖。
- `FullAccess` 移除 Haven 管理工具的 workspace 文件范围；子进程使用当前 Windows 用户权限，不会提升权限。`ExecutionWorkspace` 仍只提供默认 `cwd` 和 Files 相对路径根，不会限制任意子进程后续访问。
- `NetworkPolicy` 单独决定 Haven 是否允许网络能力及是否需要确认/目的地验证。授权后运行的 opaque 子进程仍没有 OS 级网络隔离。
- UI 将字段标为“文件访问模式”，并明确 Haven 网关约束与 OS sandbox 的区别；如果未来要在 `WorkspaceWrite` 下允许运行 opaque 子进程，必须先提供真正的 Windows 进程隔离能力及端到端文件/网络验收。

## 替代方案

- 将当前执行工作区称作 Windows sandbox：拒绝，文件句柄、cwd、环境过滤和 Job Object 都不会改变子进程的用户令牌或 OS 权限。
- 只依赖一次性确认来允许 WorkspaceWrite 下的 opaque 子进程：拒绝，确认不能把其文件和网络访问限定到 workspace。
- 现在重命名持久化/wire enum：拒绝，本切片只纠正说明与 UI 术语；wire 值不变，避免把无关配置迁移混入安全边界澄清。

## 影响与迁移

只改变 API 注释和安全设置文案，不改变授权判断、配置、IPC、数据库或执行行为；无需数据重置。用户会看到更明确的“完整用户权限”和子进程范围说明。

## 验证

- SettingsSecurity UI 测试验证“文件访问模式”标签、OS sandbox 边界说明以及 WorkspaceWrite 下 opaque 子进程被拒绝的说明。
- 运行前端类型检查与测试、Rust 格式/编译检查、ADR 索引和 `git diff --check`。
