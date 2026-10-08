# ADR 0776：Memory renderer operation 复用闭合 producer 值

## 状态

已采纳并实施（2026-10-08）。

## 背景

`MemoryTool` 的 `operation` 来自闭合 Rust `MemoryOperation`，并由 `run_with_session` 的穷尽 match 映射为 `search`、`list`、`remember`、`forget` 或 `recall`。`ToolMemoryResult` 根据其中两个值选择展示分支，其他结果通过 JsonView 呈现，但 prop 和 nested validator 都曾接受任意字符串。

## 决定

1. UI `ToolMemoryOperation` tuple 固定五种 producer 输出值。
2. renderer prop 与 ToolResult nested validator 共用该类型/guard，字段继续可选或为 null。
3. 未知动态 operation 回退通用 JsonView，以保留原始 payload 并避免误入固定分支。

## 验收与回滚

UI type check 验证 prop；ToolResultCard contract test 覆盖未知 operation fallback。Rust producer 与 wire 不变；回滚 presentation tuple、validator 和 prop 类型可恢复开放字符串行为。
