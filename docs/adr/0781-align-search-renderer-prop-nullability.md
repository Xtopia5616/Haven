# ADR 0781：对齐搜索 renderer props 与 optional-field guard 的 null 语义

## 状态

已采纳并实施（2026-10-08）。

## 背景

Builtin `ToolResult.output` 仍是动态 JSON。文件搜索和 WebSearch 的 nested guard 使用 `hasValidOptionalFields`：字段缺失或值为 `null` 都按可选字段处理；但 renderer Props 只声明了可选的非空字段。运行时接受的形状因此比静态 Props 更宽，尤其是 result row 的 `snippet` 以及搜索摘要字段。

## 决定

1. 在 Files search Props 中显式允许 guard 已接受的 `null`：`count`、`mode`、`results[].line` 与 `results[].snippet`。
2. 在 WebSearch Props 中显式允许 guard 已接受的 `null`：`label` 与 `results[].snippet`。
3. 搜索结果数组、row 必需的 `path` / `title` / `url`、`queries` 数组及非空 optional 值的类型和验证保持不变；畸形 item 仍回退通用 JSON renderer。

## 影响与回滚

仅修正动态输出边界的静态 renderer Props，不改变搜索生产者、数据或可见行为。若 guard 改为拒绝这些 null，可同步收窄 Props 回滚本决定。

## 验收

Renderer contract tests 覆盖 null optional fields 被专用 renderer 接受、错误 snippet 类型回退；运行 Svelte type check、UI 全量测试与 ADR 索引检查。
