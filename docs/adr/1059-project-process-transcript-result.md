# ADR 1059：将 process_transcript 响应投影为 renderer view

## 状态

已接受（2026-10-12）

## 背景

`process_transcript` 的 Rust `ProcessResult` 是 externally-tagged union，包含 `SessionCreated` / `Supplemented` variant 和 snake_case 的 `session_id` / `message_id`。UI command adapter 已验证 variant 和实体 ID，但随后原样返回 generated wire union；submit coordinator 因而在业务逻辑中读取 Rust 字段名，绕过了“只在 IPC 边界转换为 camelCase”的规范。

## 决定

- `parseProcessResult` 在验证 wire variant、canonical session/message ID 和 supplemented session 关联后，返回 `ProcessResultView` renderer union。
- Renderer view 使用 `kind`、`sessionId`、可选 `messageId`；session command adapter 是唯一 wire-to-view 投影边界，submit/controller 只消费 view。
- Rust response 和 generated command contract 保持不变；不要在 view 中保留原始 wire 对象。

## 影响

Session 新建/补充判断和乐观消息 ID 对齐行为不变；只调整 UI 内部返回类型和字段名，不改 IPC shape、持久化或序列化。

## 验证

- Parser 测试覆盖两种 wire variant 的 camelCase 投影和身份拒绝。
- Session command、submit/controller 测试、Svelte 检查、UI 全量测试与生产构建通过。
