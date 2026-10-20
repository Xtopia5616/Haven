# ADR 1338：Session grant 不设基于时间的自动到期

- 状态：Accepted
- 日期：2026-10-20
- 关联：[ADR 0402](0402-durable-session-authorization-grants.md)、[ADR 0425](0425-permission-grant-management-and-ui-continuation.md)、[ADR 1327](1327-recover-session-grant-after-resolve-append-failure.md)

## 背景

Session grant 与持久 session 绑定并在重开时恢复。它没有授予时间或到期字段；会话确认请求的 deadline 仅控制当前 ToolCall continuation，不能代表已提交授权的期限。授权确认 UI 和 Settings 虽然提供了会话授权查看、逐条撤销及整组清除，但没有直接说明 Session grant 不会按时间失效。

## 决定

1. Session grant 不采用基于时间的自动到期，也不增加定期复查提醒。用户确认“本会话允许/拒绝”后，决定持续有效，直到精确撤销、会话授权重置，或所属 session 被删除（包括 retention 删除）。
2. Session grant 继续由 Memory 持久化，并由 Agent 在 actor 安装、session 重开和安全策略重应用时恢复到授权引擎。Transcript rollback 与 session run 结束不撤销该 grant。
3. Pending confirmation 的 `expires_at` 继续只约束该确认请求和对应 ToolCall continuation。请求到期不删除已经提交的 Session grant。
4. ConfirmationDialog 和 Security 设置必须明确说明 Session grant 不会按时间自动到期，以及撤销/会话删除的失效边界。Settings 保留现有查看、逐条撤销和整组清除能力，供用户人工复查。
5. 不新增数据库字段、IPC 字段或迁移；当前持久化表和恢复/撤销路径满足此策略，无需重置数据。

## 替代方案

- 按固定天数自动到期：拒绝。用户选择授权按所属 session 生命周期持续有效，不增加周期性确认。
- 添加“永不过期”复选框：拒绝。Session grant 的期限统一且始终不按时间到期，避免每条 grant 出现不必要的期限分支。Always scope 仍表示跨会话授权。
- 只在 Settings 保留现有撤销能力而不披露生命周期：拒绝。确认 UI 应在用户授予时说明持续时间，Settings 也应说明长期授权的有效边界。

## 验证

- UI 回归检查 ConfirmationDialog 在持久 session 确认时显示无自动到期说明，并且无 session 的确认不展示 Session scope。
- Security 设置回归检查现有 grant 列表说明无时间到期、可撤销及 session 删除级联语义。
- Memory、Agent 与 App 既有测试覆盖 grant durable round-trip、actor/reopen 恢复、精确撤销和 reset；本 ADR 不改变这些实现和契约。

## 回滚

删除新增说明和本 ADR 即可恢复原文案。没有持久数据、IPC 或运行时行为需回滚。
