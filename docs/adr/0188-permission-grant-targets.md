# ADR 0188：确认授权的期限与目标范围分离

- 状态：Accepted
- 日期：2026-09-21

## 背景

确认弹窗原先只区分“本次 / 本对话 / 永久”，但“期限”和“授权对象”混在一起：用户无法表达“本对话允许这一功能组”，也无法确认永久授权究竟覆盖一个操作还是整个工具。拒绝菜单还存在文案范围大于实际写入 key 的漂移。

## 决策

将确认决策拆成两个独立维度：

1. `PermissionScope` 表示期限：`once`、`session`、`always`。
2. `PermissionTarget` 表示目标：`operation`、`group`、`tool`。

目标只能由当前 capability 的合法祖先计算得到：

- `operation`：完整 capability，例如 `system.power.lock`；
- `group`：最近父级，例如 `system.power`；
- `tool`：顶层父级，例如 `system`。

renderer 只提交目标类别，不提交任意权限键。后端根据待确认请求的 capability 重新解析并拒绝不存在的父级，确保前端不能扩大到无关 capability。

确认界面默认提供“本次允许”和“本对话允许此操作”，其余组合进入更多选项；永久授权或扩大目标范围需要二次确认。拒绝使用相同的目标层级，保证 UI 文案与实际策略一致。

## 安全不变量

- `deny` 仍然优先于 `allow`，且永久/会话 deny 继续受硬边界约束。
- `Critical` 操作不能通过永久允许绕过每次确认底线。
- 一次性授权不写入 session 或永久策略。
- 目标解析不改变 receipt 对完整 capability 和 canonical input 的绑定。
- capability 层级之外的字符串、兄弟节点和任意父级均不可由 IPC 请求创建。

## 验证

- `CapabilityScope::target` 单元测试覆盖操作、功能组、工具和无父级场景。
- IPC 契约将 `target` 纳入 `resolve_confirmation` 请求。
- UI 类型检查、测试和 Rust workspace 门禁验证确认路径。

## 兼容与迁移

这是测试版内部 IPC 契约变更，不保留缺少 `target` 的旧确认调用。前端默认操作目标只用于恢复层的安全兜底；正式确认请求始终由弹窗显式提交目标。
