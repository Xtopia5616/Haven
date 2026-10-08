# ADR 0787：对齐 Files 与 Input renderer Props 的 null 语义

## 状态

已采纳并实施（2026-10-08）。

## 背景

Files 与 Input 的 renderer-consumed optional scalar fields 由 `hasValidOptionalFields` 验证，缺失或 null 均按缺省值接受。Files outline row 的可选 line/name 也使用相同 nested guard，但 `ToolFileResult` 与 `ToolInputResult` Props 仅对有限 discriminator 和部分字段表达 nullable。

## 决定

1. 让 Files root scalar fields 与 outline row line/name 在 Props 中接受 guard 已接受的 null；动态 `matches` 仍是 `unknown`，entries/symbols 数组本身仍不可为 null。
2. 让 Input root chars/typed/pressed/scrolled 接受 null；点击/移动坐标仍必须是两个有限数值的 tuple，因为专用 array guard 不接受 null。
3. 保持 operation/button 的闭合值域、错误类型 fallback 与现有渲染行为。

## 影响与回滚

只修正 dynamic ToolResult renderer Props，不改 Files/Input producer 或 IPC。若 optional-field guard 收窄 null 语义，可同步移除相应 nullable unions 回滚。

## 验收

Renderer contract tests 覆盖 scalar/row optional null 被专用 renderer 接受及错误值类型 fallback；运行 Svelte type check、UI 全量测试与 ADR 索引检查。
