# ADR 0997：统一会话恢复消息身份字段

## 状态

已采纳并实施。

## 背景

`SessionResumeResponse.messages` 曾直接序列化 Memory 的 `Message`，因此把持久实体的身份暴露为泛名 `id`。该消息可能使用 `msg-*`，也可能按 transcript 关联规则复用 `step-*`。UI 只能依靠字段所在对象推断 ID 含义，且 App 没有拥有这个 IPC DTO。

## 决定

1. App 定义 `SessionResumeMessageDto`，wire 身份字段使用 `message_id`；由 `Message.id` 显式映射到该 DTO。`messages[].session_id` 等其他字段继续由 App 映射。
2. UI resume contract 只接受 `message_id`，并按 ID 与 `SessionStep.step_id` 关联；渲染消息在边界映射为现有 `id`。
3. `message_id` 允许规范的 `msg-*`，以及由对应恢复步骤锚定的 `step-*`。不以内容相等推断身份。
4. Memory `Message.id` 和 SQLite `messages.id` 保持 owner 内部形状；本决定不改数据库、durable event 或消息持久化语义。
5. 本决定仅 supersede ADR 0990 中“IPC wire shape 不变”对 resume `messages[].id` 的适用范围；其他命令和事件 wire 字段保持原约定。

## 影响与验证

App command adapter 拥有 resume message wire DTO，IPC 生成类型、UI parser 与 renderer 投影同步更新。旧的仅含 `id` 的 resume message 不再通过 UI contract。随应用一起发布的 Tauri response 原子更新，不需要数据库重置或持久数据迁移。

验证通过 Rust workspace 测试、Clippy、格式检查、UI 类型检查与全量测试、IPC command/event contract checks、ADR 索引检查和差异空白检查。

## 回滚

Rust DTO 和 UI consumer/parser 应作为同一版本恢复为 `Message.id` wire 字段并重新生成 IPC 类型。数据库与 durable event 无需回滚。
