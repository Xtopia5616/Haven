# ADR 0754：Admin session renderer 状态复用 SessionStatus

## 状态

已采纳并实施。

## 背景

`AdminServices::sessions` 将 `SessionSummaryOutput.status` 序列化为 Common `SessionStatus`，值域与会话持久状态相同。`ToolAdminResult` 却将 session/error rows 中展示的 `status` 声明为开放字符串，builtin admin guard 也只验证字符串，使未来值或畸形值可作为会话状态直接进入 UI。

## 决定

- Admin session row 的 `status` 使用 generated `SessionStatus`；非空字段由共享 `isSessionStatus` guard 严格校验，未知值回退原始 JSON。
- `contracts/session.ts` 导出该生成值 guard，并由 lifecycle mapper 复用，避免两处维护同一枚举判断。
- `title` 明确允许 `null`，对应 Rust `Option<String>` 序列化值。

## 影响与验证

只收紧 builtin admin 结果的 UI renderer contract，不改变 Rust producer、ToolResult JSON、IPC 或持久化。新增未知状态 fallback 与有效 `running` 状态保留专用 renderer 的用例。

## 回滚

恢复开放字符串 status 与旧 guard 即可。没有数据、IPC 或持久化迁移。
