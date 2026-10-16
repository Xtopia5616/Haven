# ADR 1217：按受影响工具收窄进程设置撤销

## 状态

已实现。全局授权 revision 继续使旧 proof 失效；`tool_settings` 变更只终止使用发生变化设置的进程。全局策略和永久 trust 规则仍执行全局进程撤销；单会话 trust 撤销由 [ADR 1218](1218-scope-process-revocation-to-session-trust.md) 按 session 处理。

## 背景

ADR 1211/1212 将 Shell、后台 Shell ToolRun 与 Skill 子进程接入授权撤销。此前所有 `policy_revision` 更新都通过同一个 signal 通知所有进程，因此修改一个工具的超时、输出上限或开关也会中断其它 Shell/Skill 进程。

`tool_settings` 是按工具及工具根别名应用的配置；一次变更能够明确识别受影响的工具集合。permission mode、永久规则、网络/sandbox 边界和 execution workspace 仍由全局授权状态控制，不能缩小它们现有的撤销范围。授权 proof 也必须继续对任何设置变化失效，即使正在运行的其它工具无需停止。会话 trust 是另一明确撤销域，见 ADR 1218。

## 决定

- 将 proof 新鲜度与进程终止信号分开。proof 继续绑定 `policy_revision`；实时 permit 在副作用前读取当前 revision，因此任何工具设置变更仍拒绝旧 proof。
- `AuthorizationEngine` 保留全局进程撤销 signal，供 permission mode、永久 trust、sandbox/network boundary、execution workspace 及其它全局安全变化使用。单会话 trust 的进程撤销见 ADR 1218。
- 每个 proof-derived execution context 注册一个弱引用的工具设置取消 token，并按 `tool_setting_names` 登记具体工具与已有根别名。engine 不持有执行 context 的强引用；注册表在登记/撤销时清理失效条目。
- `set_tool_settings` 比较更新前后的配置，只收集值实际变化的键；在持有配置写锁时推进 policy revision 并取消相应工具 token，不发布全局进程撤销 signal。持锁顺序保证旧 context 要么先注册并被取消，要么在新配置发布后才捕获 context，不会漏撤销新旧交界处的进程。
- Shell 与 Skill 的现有生命周期监听同时等待全局撤销 signal 和自身工具 token。比如 `skill.demo` 绑定到 `skill` 根设置；更新 `shell` 不会停止该 Skill，而更改 `skill` 会终止它。
- 定向撤销覆盖正在运行的进程，不是会话级或单次调用级许可重写。其它 session 中使用相同受影响工具的进程也会停止；未受影响工具继续运行。

## 替代方案

- 保持所有工具设置更新都全局撤销：拒绝；无关设置会中断长时间命令，且受影响工具集合已有稳定配置身份可以精确定位。
- 只递增 proof revision 而不停止受影响进程：拒绝；正在运行的目标工具会继续按已撤销配置执行。
- 对每个工具维护独立 proof revision：暂不采用；全局 revision 已为授权 receipt、spawn permit 和文件副作用提供统一失效契约。独立 process token 已足以降低这次确认的误中断范围。
- 会话 trust 撤销不放进工具注册表；它有独立授权 owner 与 token，按 ADR 1218 单独处理。

## 影响与迁移

改动仅涉及 Tools 内存中的授权 revision、撤销 signal 与弱 token 注册表，无配置、IPC、数据库或持久字段变化，无需重置或兼容迁移。全局安全撤销仍可能终止其他 session 的 Shell/Skill；工具设置变更会终止所有使用受影响工具设置的进程。单会话 trust 撤销只终止所属 session 的进程。

## 验证

- 定向回归验证工具设置变化推进 policy revision 并拒绝旧 proof，但不发布全局进程撤销 signal。
- 定向回归验证修改 Shell 设置会终止 Shell，保持 Skill 根设置不变时 Skill 继续运行；之后修改 Skill 根设置会终止绑定具体 Skill 名称的进程。
- Registry 回归验证过期的动态工具名会在后续登记时清理，不随已结束的 execution context 累积。
- Security 授权测试验证全局策略撤销仍唤醒运行中授权。
- `haven-tools` locked 全量测试、严格 Clippy、格式检查、ADR 索引和差异检查覆盖本切片。

## 回滚

移除工具级 token 注册表和取消分支，让 `tool_settings` 更新恢复发布全局撤销 signal；保留 ADR 1218 的 session trust 撤销域和 proof revision 检查。没有持久数据需要恢复。
