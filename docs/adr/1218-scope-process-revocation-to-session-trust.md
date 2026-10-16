# ADR 1218：按 session 收窄 trust 撤销的进程范围

## 状态

已实现。单会话 grant revoke/clear 仍推进全局授权 revision 以拒绝旧 proof，但只终止绑定该 session 的 Shell/Skill 进程；全局策略变化仍全局撤销。

## 背景

ADR 1211/1212 将 Shell 与 Skill 执行 proof 绑定到进程撤销 signal。session grant 删除也推进全局 `policy_revision`，此前会终止所有 session 的运行进程，即使 trust 只属于一个 session。ADR 1217 已将 `tool_settings` 更新从全局进程撤销中分离，session trust 需要同样明确的撤销域。

执行 proof 的不可变 binding 已包含 `session_id`，而授权上下文由 Tools 在验证 proof 后创建。因此进程 owner 能从已验证身份登记 session token，不需要接受 renderer/session 参数作为额外授权输入。revision 仍是所有 proof 的 freshness gate；仅已运行副作用的停止范围变窄。

## 决定

- `execution_context_for_proof` 从已验证的 proof binding 取得 `session_id`，在创建 side-effect authorization 时登记 session-scoped cancellation token。缺少 session identity 的执行不加入 session registry。
- `revoke_session_grant` 和 `clear_session_trust` 在实际移除该 session grant 时推进全局 `policy_revision`，使旧 proof、receipt 和尚未完成的副作用 permit 失效；它们只取消映射到该 session 的运行进程，不发布全局进程撤销 signal。
- `clear_all_trust` 仍推进全局 revision，但只取消当前 session grant map 中存在的 sessions。没有被清除 trust 的 session 进程继续运行。
- permission mode、`apply_security`、sandbox/network boundary、execution workspace、永久规则 revoke/clear 继续通过全局进程撤销 signal 停止所有 proof-bound process。切换 mode 或边界即使清空 session grants，也仍遵循对应全局撤销范围。
- 全局授权 revision 变化仍会拒绝任何旧 context 的下一次受保护副作用。运行中的其它 session 进程可继续当前已启动工作，但不得再使用旧 proof 启动后续副作用。
- session token 注册表仅持有 weak cancellation references，并在登记时清理失效条目。session revoke 与 execution context capture 都持有 config lock 后再访问 registry，保证更新前登记的 context 被取消，更新后创建的 context 捕获新策略。
- 撤销粒度保守到 session，而非单 capability：一个 session 的 trust revoke 会停止该 session 的所有 proof-bound process。session grant 可按父 capability 匹配多个子操作，当前不保留“某条 grant 对某个进程是否实际起效”的依赖索引。

## 替代方案

- 保留全局进程撤销：拒绝；单会话信任修改会中断完全无关的其它会话。
- 只提高 policy revision，不停止被撤销 session 的进程：拒绝；已运行的 Shell/Skill 会在信任撤销后继续产生副作用。
- 按单 capability 撤销进程：暂不采用；父/子 capability 规则可能共同参与授权，当前没有可证明的运行进程到具体匹配 grant 的依赖关系。
- 不推进全局 revision：拒绝；旧 proof、confirmation receipt 和 spawn/side-effect permit 会在 session grant 变化后仍可复用。

## 影响与迁移

变化限于 Tools 进程内 session→weak cancellation token 索引和撤销 signal 分层；不增加配置、IPC、数据库或持久字段，无需重置或兼容迁移。单会话信任收回会停止该 session 的前台/后台进程；其它 session 的当前进程不被打断，但其旧 proof 不能继续启动新的副作用。

## 验证

- 回归通过真实 `AuthorizationRequest → AuthorizationExecutionProof → ToolExecutionContext` 链验证 token 绑定 session identity。
- 测试覆盖单 grant revoke、单 session trust clear、clear-all trust，以及 permission mode 继续全局撤销；受影响 session 的运行授权结束，其它 session 在相应操作下继续运行。
- registry 测试验证过期 session identity 在后续登记时清理。
- `haven-tools` locked 全量测试、严格 Clippy、格式检查、ADR 索引与差异检查覆盖本切片。

## 回滚

移除 session-scoped token registry 与监听分支；在 session grant revoke/clear 时恢复发布全局进程撤销 signal。保留全局 policy revision 更新。没有持久数据需要恢复。
