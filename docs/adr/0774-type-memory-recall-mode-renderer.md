# ADR 0774：Memory recall mode renderer 复用闭合值

## 状态

已采纳并实施（2026-10-08）。

## 背景

`MemoryRecall.mode` 是 Rust 闭合 enum，Serde 输出 `keyword` 或 `hybrid`；`MemoryTool::recall_output` 将该 typed 值序列化到 `ToolResult.output`。UI 的 Memory renderer 仅展示该值，但 prop 和 builtin nested validator 都把它放宽为任意字符串。

## 决定

1. UI presentation contract 定义 `ToolMemoryRecallMode`，取值为 `keyword` / `hybrid`。
2. `ToolMemoryResult` prop 与动态 ToolResult validator 共用该 guard；mode 继续可省略。
3. 未知动态结果回退通用 JsonView，不把未知 mode 误当成稳定展示值。

## 验收与回滚

UI type check 验证 renderer props；ToolResultCard contract test 验证未知 mode 回退。Memory Rust enum 与序列化不变；回滚 UI 类型和 validator guard 即可恢复开放字符串展示。
