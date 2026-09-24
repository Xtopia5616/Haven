# ADR 0271：llm_usage.call_kind 运行时类型边界

- 状态：Accepted
- 日期：2026-09-24
- 范围：Tools、Agent、Memory 的 LLM usage runtime input
- 关联：[ADR 0124](0124-media-usage-cache-boundary.md)、[ADR 0270](0270-typed-cache-accounting.md)

## 背景

`agent`、`media`、`tool` 在工具结果、ReAct 处理、Agent usage 更新和
`LlmCallUsageInput` 中重复以字符串传递。其合法值由 `llm_usage.call_kind` 的既有 schema
约束，但跨层输入允许拼写错误或未经登记的值。数据库读模型和 `agent:usage` IPC 已有稳定的
字符串字段，本次不需要改变这些边界。

## 决策

在 `haven-common` 定义带 snake_case serde 表示的 `LlmCallKind`，提供 `as_str()` 与返回
`Option` 的 `parse()`。`ToolLlmUsage`、Agent `UsageUpdate`、ReAct 内部 usage 路径和
`LlmCallUsageInput` 使用该 enum。Agent 累计用量入口只接受 `Agent`。

SQLite 查询/写入以及 durable `LlmCallUsage` 投影在存储边界使用既有字符串。读出的 usage
类别只在重新进入 memory 聚合输入时解析；durable `LlmCallUsage` 自身仍保留 `String`，不做
数据库迁移或重写。`UsagePayload` 和 `AgentEvent::Usage` 继续用字符串并在 IPC 边界转换。
schema 的默认值和 CHECK 约束、事件字段名与 JSON 类型、前端 contract 均保持不变。

保持数据库读模型和 IPC 字段为 `String`，避免让已有 durable/read 或 wire 契约依赖 enum 的
反序列化行为。把转换扩展到这些边界之外会扩大兼容范围，却不能减少 runtime input 的重复
字符串，因此不纳入本切片。

## 影响与验证

- 工具侧 usage producer 和 Agent/Memory runtime inputs 在编译期限制为三个已知类别；
- `LlmCallKind` 单测覆盖 snake_case serde、`as_str()`、合法/非法解析；Agent usage 测试覆盖
  累计入口拒绝非 `Agent`，tool/media usage 投影继续验证既有 JSON 字符串值；
- `cargo fmt`、相关 crate check/test 与严格 Clippy 验证实现；无需数据库重置。

## 回滚

可回退本 ADR 涉及的 enum 字段与边界转换，恢复 runtime `String` 输入；数据库和 IPC 无需迁移或
重置。
