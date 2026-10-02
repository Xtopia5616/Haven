# ADR 0425：权限规则管理与界面直调续体

- 状态：Accepted
- 日期：2026-10-02
- 关联：ADR 0109、0196、0402、0423、0424

## 背景

权限设置只展示永久 allow/deny，但 `reset_permissions` 同时删除持久会话 allow/deny；永久规则的撤销也会移除其他会话中同 capability 的 grant。用户看不到这些副作用，也不能按具体会话检查或撤销 grant。会话 allow 与 deny 都是持久安全决定，必须同等可见。

界面直调确认把 `ui` 作为展示 session id。它在持久化操作前就从待确认 map 移除请求；若 receipt 校验或配置写入失败，renderer 和后端都无法重试。获批 MCP、Skill、Admin 操作还会在 resolve IPC 内执行，长操作延迟确认弹窗收起。

## 决定

1. `reset_permissions` 只清除永久规则；`revoke_permission` 只撤销永久规则。两者保留会话 grant。
2. 新增 `list_session_permissions`、`revoke_session_permission(session_id, capability)` 与 `reset_session_permissions`。列表返回会话 id/title、capability、target 和 allow/deny；逐项撤销只匹配指定会话和 capability；整组重置只清持久会话 grants。
3. 设置页分别显示永久规则和会话授权，清除确认框明确列出影响数量和范围。会话 deny 与 allow 一样可见、可撤销。
4. 无持久 conversation owner 的 renderer 确认只允许 Once 或 Always。UI 隐藏 session scope；后端拒绝伪造的 session grant，也不再将展示 session id 用作授权持久化主体。
5. renderer 确认在 receipt 验证和持久化决定成功前保持 pending。可恢复错误保留请求；接受决定后立即发出 resolved lifecycle、从 pending owner 移除，并把获批动作作为应用任务执行。动作失败只报告结果，不重新打开已消费的授权请求。
6. session grant 的精确撤销同时更新该 session 的 live allow/deny map；永久 grant 操作不清理任何 session map。重置只清对应类别的持久与 live 状态。

## 安全与替代方案

- 选择单独命令保留现有 `list_permissions` DTO，避免把永久和会话授权混成一个无 owner 的平面列表。
- 不用全局 capability 删除来实现单条会话撤销，避免无意撤销其他会话的 deny 或 allow。
- Always 决定先持久化配置，再安装 live grant；失败时请求仍可操作。UI action 执行阶段不再持有确认 map，也不能重复消费同一 request id。
- 新增 app-scoped continuation 使用已有 ApplicationRuntime 关闭边界，不新增持久交互表或 transcript 写路径。

## 兼容与重置

只新增 Tauri commands，不改变数据库 schema；现有会话授权表已包含 session id、capability、target、effect。配置与数据库均无需重置。前端和后端需随同一版本发布。

## 验证

验证精确会话撤销、永久 reset 保留会话 grants、会话 reset 保留永久规则、allow/deny 列表投影，以及 UI-only confirmation scope 与 resolve retry 语义。更新 command/event 契约登记并执行仓库 Rust、IPC 和 UI 门禁。
