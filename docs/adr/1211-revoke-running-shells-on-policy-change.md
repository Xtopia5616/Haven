# ADR 1211：策略撤销时终止运行中的 Shell 进程树

## 状态

已实现，撤销范围由 [ADR 1217](1217-scope-process-revocation-to-changed-tool-settings.md) 细化：全局安全策略撤销仍终止前台 Shell 与后台 Shell ToolRun；`tool_settings` 变化只终止受影响工具的进程。

## 背景

[ADR 1199](1199-revalidate-live-policy-at-process-spawn.md) 为进程创建提供实时策略读 permit，保证策略更新与 OS spawn 有明确顺序，但它只覆盖启动线性化点。进程开始运行后，切换 permission mode、sandbox/network boundary、execution workspace、tool settings，或撤销/清除信任会递增全局 `policy_revision` 并使新的调用 proof 失效；已运行 Shell 此前不会收到撤销信号。原实现把 `tool_settings` 也合并到全局进程撤销；ADR 1217 已将它改为按变更工具定向撤销。

进程启动是持续副作用。用户收紧边界后让旧 Shell 继续运行，会使当前授权设置与真实进程能力脱节。另一方面，长期持有 policy read lock 会阻塞设置更新，因此运行期撤销不能通过锁住整段命令执行实现。

## 决定

- `AuthorizationEngine` 是策略 revision 和进程撤销信号的唯一 owner。proof 派生的 `ExecutionPolicyAuthorization` 在持有配置读锁时捕获当前进程撤销 token，避免授权快照与 signal 脱节。
- 权限 grant 不发布撤销 signal：授予权限不会撤销当前已授权的工作。全局配置/边界变更、workspace 切换、trust 清理和显式 revoke 继续递增全局 revision，并使旧 proof 及运行中的授权 Shell 失效。`tool_settings` 仍递增 proof revision 以拒绝旧 proof，但进程撤销按受影响工具定向处理，细节见 ADR 1217。
- 前台与后台 Shell 持有短期 policy read permit，直到 child 已接入进程 containment 且 Windows 初始线程已恢复。之后立即释放 permit，不让长时间命令阻塞设置更新。
- 两类 Shell 在进程运行期间监听捕获的全局撤销 token 或其工具设置 token。撤销后 Shell 终止主进程并关闭 Job Object；前台调用返回 `Cancelled`，后台 ToolRun 进入 `cancelled`。用户取消与应用关闭继续走原有进程树回收路径。
- signal 使用独立 token 身份而不是仅比较 `u64` revision，因此即使 revision 达到饱和值，之后的策略变更仍可通知已运行进程。
- 对全局策略和 trust 撤销，行为仍是保守的全局撤销：相关变更会终止所有 proof 绑定的前台/后台 Shell，即使变更只针对一个 session。工具设置的撤销域已由 ADR 1217 收窄到设置实际影响的工具；这不改变权限模式、执行 workspace、sandbox/network 边界和 trust 撤销的全局行为。
- 本 ADR 只接通 Shell 与其后台 ToolRun。Skill、MCP stdio 及其他子进程 owner 仍需各自把进程 lifecycle 连接到相同授权撤销 owner；不能把本次 Shell 行为描述为全体子进程均已支持撤销。

## 替代方案

- 进程启动后忽略策略更新：拒绝；旧 Shell 会在用户收紧权限后继续产生副作用。
- 持有 policy read lock 到进程退出：拒绝；长时间命令会阻塞权限撤销和安全设置。
- 只在 `spawn` 返回后读取一次 revision：拒绝；检查与 attach/resume 之间有竞态，也不能接收后续变更。
- 逐 session/capability 杀进程：暂不采用；当前授权 proof 的实时失效权威是一个全局 revision，局部撤销索引与持久会话所有权尚未提供可审计的运行进程映射。当前选择 fail closed 的全局撤销，并公开其影响范围。

## 影响与迁移

新增仅存在进程内的撤销 signal 和工具 token，不增加配置、IPC、数据库或持久字段，无需重置或兼容迁移。全局策略收紧会终止所有绑定的 Shell；工具设置变化只终止依赖受影响设置的 Shell。其结果按既有 cancellation 契约表示；后台 ToolRun 为 `cancelled`。

## 验证

- 授权单测验证策略修改会通知已运行授权。
- Windows adapter 回归测试验证前台 Shell 与后台 ToolRun 的后代进程都随策略撤销退出，且分别报告 cancellation 状态。
- Tools crate locked tests/clippy、workspace tests/clippy 与格式检查作为本切片门禁。
- Skill/MCP stdio 自身对策略撤销的生命周期验收不属于本切片，后续接线前必须单独补负向与进程树回收测试。

## 回滚

移除 `AuthorizationEngine` 的撤销 signal、proof 捕获 token 及前后台 Shell select 分支，恢复仅在进程启动点校验策略的行为；保留 ADR 1199 的 spawn permit。没有持久数据需要恢复。
