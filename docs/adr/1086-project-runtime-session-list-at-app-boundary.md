# ADR 1086：在 App 边界投影运行时会话列表

## 状态

已接受（2026-10-13）

## 背景

`list_runtime_sessions` 曾直接序列化 Agent 的 `SessionInfo`，把内部 summary、步骤及其 JSON input/output、创建和更新时间一并暴露到 Tauri IPC。会话切换器只需要会话身份、首条输入、标题、状态和等待原因。这也违背 ADR 0003 对直接暴露 Agent `SessionInfo` 的拒绝决定。

## 决定

- App 定义 `RuntimeSessionDto` 作为命令响应 allowlist，并在 handler 中显式从 Agent `SessionInfo` 投影。
- Rust wire 字段使用 `session_id`、`input_text`、`waiting_reason`；UI 在契约边界映射为 `sessionId`、`inputText`、`waitingReason`。
- 摘要、步骤、工具输入/输出及运行时创建/更新时间保留在后端，不属于运行时列表 IPC。
- Rust DTO 仍由 IPC 生成器作为唯一 wire shape 来源，UI parser 校验完整 shape 与语义约束。

## 影响

`list_runtime_sessions` 返回字段变窄并统一首条输入名称。该命令是当前进程内的 UI 投影，不涉及持久化数据或数据库 schema；随 Rust DTO 更新生成的 TypeScript 契约和 UI parser。

## 验证

- Rust 序列化回归断言 Agent 内部 summary/step 数据不会进入 wire response。
- UI parser 回归覆盖 required fields、closed enums、canonical session ID、等待原因语义和 camelCase 映射。
- `scripts/check-ipc-contracts.ps1` 和完整 Rust/UI 门禁。

## 回滚

不要直接重新暴露 Agent `SessionInfo`；如切换器确需新增字段，应在 App DTO 中评估并显式加入 allowlist。
