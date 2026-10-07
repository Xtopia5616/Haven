# ADR 0724：Resume usage record 复用 LlmCallKind

## 状态

已采纳并实施。

## 背景

ADR 0723 已让 live `agent:usage` 事件从 Agent 到 UI 共用 `LlmCallKind`，但继续阅读持久恢复路径时发现 `Memory::LlmUsageRecord.call_kind` 仍是 `String`，`session_events` 中复用该记录的 usage payload 也继承自由字符串。UI `SessionLlmUsage` 再显式覆盖 generated `LlmUsageRecord` 的字段为 `string`。

这些值由 `LlmUsageRecordInput.call_kind: LlmCallKind` 写入，而 schema 为 SQLite 列限制同样的三种值。Memory 读取路径只是在列边界拿到原始文本，并将 `LlmUsageRecord` 暴露给 durable projection、session resume 与 IPC；SQL 字符串并非这些业务 DTO 的契约理由。ADR 0271 对旧 reader/wire 的容忍决定由本 ADR 在该读模型范围内 supersede。

## 决定

- Memory `LlmUsageRecord.call_kind` 改为 `LlmCallKind`；所有由 typed input 生成的记录直接复制该 enum。
- Memory 从 SQLite 读取 `call_kind` 时，严格解析为 `LlmCallKind`；无效值返回 `FromSqlConversionFailure`，不再作为任意字符串透传或在聚合中静默忽略。
- 写 SQLite 时仍通过 `as_str()` 写入原有 text 列；schema、CHECK 值域、列名和持久值保持不变。
- IPC generator 由 `SessionResumeResponse → LlmUsageRecord` 自动生成同一闭合 enum；UI `SessionLlmUsage` 不再把该字段覆写为 `string`。
- 与 ADR 0723 合并后，runtime、live event、durable usage event payload、resume projection 的 Rust 类型都以 `LlmCallKind` 为唯一 owner；SQLite 原始列字符串仍为存储细节。

## 替代方案

- 仅在 UI 对 `LlmUsageRecord.call_kind` 添加手写 union：拒绝。Rust resume response 与 event payload 仍会保留自由字符串。
- 把 SQLite 列改为不同编码或重写历史行：拒绝。Serde enum 保持现有 snake_case 字符串，SQL text 与 CHECK 无需 schema 变更。

## 影响与验证

`SessionResumeResponse.llm_usage[].call_kind` 的生成 TypeScript 从 `string` 收窄为 `LlmCallKind`。有效 SQL 和 event JSON 的值不变；非法存储文本会在 Memory 边界失败。数据库 schema 与历史合法数据无需迁移或重置。

按跨 crate/IPC 契约门禁运行 Rust workspace fmt/check/Clippy/tests、UI check/tests/build、IPC generator/drift、event checks、ADR index 与 diff checks。

## 回滚

恢复 `LlmUsageRecord.call_kind: String` 与 UI override，并恢复从 Memory row 读取原始字符串。SQLite 数据、schema 与事件中现有值无需回滚。
