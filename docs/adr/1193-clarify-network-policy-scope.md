# ADR 1193：明确网络策略只约束 Haven 工具授权入口

> `NetworkPolicy::Ask` 下通过确认后运行的 WorkspaceWrite Shell 由 MXC 强制网络策略，见 [ADR 1368](1368-mxc-processcontainer-for-workspace-shell.md)；其它 opaque 子进程仍按本文拒绝或要求 Open。

## 状态

已采纳并实施。

## 背景

`NetworkPolicy` 决定 Haven 是否为工具操作签发执行授权，但不创建 Windows 防火墙规则或其它系统级网络限制。网络设置中的简短说明容易让“禁止网络”“受限网络”被理解成 OS 全局封锁；获准启动的宿主子进程仍按当前 Windows 用户权限联网。

## 决定

- 明确四种策略的作用点是 Haven 工具授权网关：`Deny` 拒绝声明需要网络或目的地不透明的操作；`Ask` 对可验证公网能力请求确认，opaque capability 仍需用户确认并且在 `WorkspaceWrite` 下始终拒绝；`Restricted` 只允许 Haven 能验证目的地的网络能力并拒绝 opaque capability；`Open` 不增加全局网络封锁或确认，但工具自身授权要求继续生效。
- UI 对每种策略说明对应的 Haven 行为，并明确这些选择不是 Windows 防火墙规则，不改变其它程序或已经运行的进程的网络权限。
- `FullAccess` 下获准启动的子进程仍使用当前 Windows 用户 OS 网络权限。`NetworkPolicy` 不限制 Shell、Skill、MCP server 或其后代可访问的地址；按目的地控制任意进程网络仍需要单独的 OS 级隔离设计。
- 本 ADR 只澄清现有运行语义，不新增权限值、不改变配置/IPC/数据库或授权判断。

## 替代方案

- 将 `Deny` 或 `Restricted` 描述成 Windows 全局网络封锁：拒绝，当前实现只约束经过 Haven 工具授权链的操作。
- 把 `Open` 解释为跳过工具确认：拒绝，操作 effect、risk、permission grant 和工具专属确认契约仍会检查。
- 把环境变量过滤描述成网络隔离：拒绝，过滤代理变量不会限制进程直接建立 socket 或使用其它网络 API。

## 影响与迁移

仅更新安全设置文案、UI 回归测试和架构路线图，无配置、IPC 或持久状态变化，无需数据重置。用户可以更准确地区分 Haven 的操作授权和 Windows OS 网络权限。

## 验证

- SettingsSecurity 测试逐一切换 `Deny`、`Ask`、`Restricted`、`Open`，检查每种模式的文案都准确说明授权范围。
- 运行前端类型检查、前端完整测试、格式检查、ADR 索引和 `git diff --check`。

## 回滚

还原网络策略文案和回归测试，删除本文及路线图中的 ADR 1193 引用。无需恢复配置或持久数据。
