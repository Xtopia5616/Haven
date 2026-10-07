# ADR 0692：按 Session 命令标明 request alias owner

## 状态

已采纳并实施。

## 背景

前端 `SessionIdRequest` alias 实际由 `reopen_session` 的 generated request 派生，却被 Session command wrappers 和 `ChatSessionController` 复用于 `get_session_lineage`、`get_session_for_resume`、`delete_session`、`end_session`、`interrupt_session` 与 `continue_session`。这些请求当前形状相同，但 alias owner 与六个消费者的命令不对应；若单个 Rust handler 参数变化，错误的别名不会表达真实契约来源。

ADR 0681 已规定 UI command request alias 应由实际调用的 generated command 派生，即使字段形状相同也保留 command owner。

## 决定

- 为 `get_session_lineage`、`get_session_for_resume`、`reopen_session`、`delete_session`、`end_session`、`interrupt_session` 与 `continue_session` 分别声明 request alias。
- 将每个 wrapper/controller 的 request 参数绑定到它实际调用的 generated command；删除 `SessionIdRequest`。
- 保持 camelCase 字段、Rust/Tauri 序列化和运行行为不变。

## 替代方案

- 继续共享 `{ sessionId: string }` alias：拒绝。字段相同不代表 command owner 相同，且现有命名规则已要求按调用命令派生。
- 为请求统一创建共享 DTO：拒绝。Rust command registry 已经逐个拥有 request shape，新增共享 DTO 会形成第二个契约来源。

## 影响与验证

- 仅调整 renderer TypeScript 类型 alias 和调用点，不改变 IPC 字段、Rust handler、持久化或用户可见行为。
- 验证：`corepack pnpm run check`、`corepack pnpm run test:run`、ADR 索引检查和 `git diff --check`。

## 回滚

恢复 `SessionIdRequest` alias 和旧消费者即可；不涉及数据或 wire 迁移。
