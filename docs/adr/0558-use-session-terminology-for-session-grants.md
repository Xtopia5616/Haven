# ADR 0558：会话授权作用域统一使用 Session 术语

## 状态

已采纳并实施；Rust workspace 门禁和 UI 门禁均通过。

## 背景

`PermissionScope::Session` 的授权规则以 `session_id` 为 key 持久化，在会话授权页中也按 `session_title` 展示并可逐条撤销。确认弹窗和安全设置却显示“本对话允许/拒绝”；App 校验错误、Memory/Tools/Agent 注释也把这类规则描述为 conversation-scoped。这样会掩盖授权的实际边界：规则随一个 persisted Session 生效，范围不是当前消息轮次或任意对话文本。

## 决定

1. 将确认弹窗的 session-scope allow/deny 操作文案改为“本会话允许/拒绝”，将菜单分组显示为“本会话”。
2. 将安全设置中已保存授权的效果标签与空状态说明统一为“本会话允许/拒绝”。
3. 将 App 校验错误、Memory/Tools/Agent/App 中描述 session grant 生命周期和 owner 的注释统一为 session/persisted session。
4. 更新相关 UI 与 Rust 测试断言；不改变 `PermissionScope::Session`、grant key、授权读写条件、确认路由、错误分类或撤销行为。

## 替代方案

- 保留“本对话”作为口语化文案：拒绝。授权表按持久 session 展示，且决议被 `session_id` 限定；UI 已使用“会话授权”作为此作用域的标题，操作词应保持一致。
- 通过更改权限枚举或授权存储结构消除歧义：拒绝。现有数据边界和行为已一致，问题只在面向用户的标签和说明文字。

## 影响与验证

- 仅修改授权范围文案、错误文字和注释，不改变数据库字段、配置、IPC payload 或安全决策。
- 验证 Rust workspace fmt/check/strict Clippy/tests，UI check/tests/build 与 diff 检查。

## 回滚

恢复原文案、错误文字和注释并同步调整测试断言；无需迁移或重置授权数据。
