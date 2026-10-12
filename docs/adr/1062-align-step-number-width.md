# ADR 1062：统一持久化与 UI 的 step_number 数值域

## 状态

已接受（2026-10-12）

## 背景

ReAct、durable session event、rollback metadata 和 IPC wire DTO 已将 `step_number` 定义为 `u32`。Memory 的 `SessionStep`、`ToolStepWrite`、`LlmUsageRecord` 与写入 API 仍使用 `i32`，Agent 在工具步和 usage 持久化前执行 `u32 as i32` 窄化；恢复事件时又将 usage `i32` 转回 `u32`。UI 的 runtime/history/usage 校验同样把 step number 当作 `i32`。因此，同一个步数超过 `i32::MAX` 时，持久化会变成负数，UI 也会拒绝其余层可表达的合法值。

## 决定

- Agent、Memory、Rust IPC DTO 与 UI 对 `step_number` 一律使用非负 `u32`；独立的 `tool_index` 保持 `i32`。
- SQLite 的 `INTEGER` 可承载完整 `u32` 范围，因此不更改列类型或 schema 版本；typed writes 直接写 `u32`，所有持久化读取严格解码 `u32`，拒绝负值及超范围值，不将损坏的步数静默映射为缺省值。
- UI 的 runtime step、resume step 和 usage parser 使用 `isU32`；tool index 继续使用 `isI32`。

## 影响

`u32::MAX` 可在步骤、usage 和事件元数据中保真往返；负数投影在读取边界显式失败。既有正 `i32` 行与序列化格式兼容，无 schema 变更、无需重置数据库。

## 验证

- Memory 测试覆盖步骤和 usage `u32::MAX` 写读往返、负数行拒绝，以及事件步数范围和负数拒绝。
- UI contract 测试覆盖 runtime/resume/usage 步数大于 `i32::MAX` 的接受和超出 `u32::MAX` 的拒绝。
- Workspace Rust 检查、clippy、全量测试，UI check 和全量测试通过。
- ADR 索引与本地链接检查通过。
