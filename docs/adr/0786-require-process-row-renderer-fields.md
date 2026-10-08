# ADR 0786：要求 Process renderer 的完整列表行字段

## 状态

已采纳并实施（2026-10-08）。

## 背景

`ProcessTool` 对每条列表记录都输出 `name`、`pid`、`cpu`、`memory` 与归一化后的 `status`。renderer 表格直接展示前四项并调用状态 badge，但 nested validator 和 `ProcessEntry` alias 将这些值声明为 optional，使 `{processes:[{}]}` 仍进入专用表格并显示缺项。

## 决定

1. `processes[]` 必须包含字符串 `name`、字符串或数值 `pid`、有限数值 `cpu`/`memory` 和已知 `ToolProcessStatus`。
2. `ProcessEntry` alias 与 validator 共用该 required row shape；缺失字段、错误类型或未知 status 回退 JSON。
3. Root operation/killed 继续遵循 optional-field guard 的 null 语义。

## 影响与回滚

仅收紧 UI renderer guard 与 alias，不改变 ProcessTool 输出。producer 仍输出完整字段，未知动态数据会保留在 JSON fallback；可将 row guard 与 alias 改回 optional 回滚。

## 验收

Renderer contract tests 覆盖缺失 row field fallback 与完整 producer row 选择；运行 Svelte type check、UI 全量测试与 ADR 索引检查。
