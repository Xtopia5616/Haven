# ADR 0788：对齐 Media 与 Window renderer 的 nested null 语义

## 状态

已采纳并实施（2026-10-08）。

## 背景

Media 与 Window renderer guards 通过 `hasValidObjectFields` / `hasValidOptionalFields` 将可选 record 字段与 scalar null 当作缺省处理。组件 Props 曾未覆盖这些 root 与 nested field nullability。`media.content` 是动态载荷，仅在其为字符串时作为文本展示；available-representations 与 window/element 数组的结构仍有专门校验。

## 决定

1. 对齐 Media root scalar fields、nested `media` object 与其 optional scalar fields；`media.content` 保持 `unknown`，available-representations array 不接受 null。
2. 对齐 Window root fields、nested `media`、`windows[]` 与 `elements[]` optional consumed fields；record arrays 不接受 null item。
3. 保持 operation、format、condition、representation、file kind 与 UI Automation control type 的闭合集合验证不变。

## 影响与回滚

仅修正动态 ToolResult renderer Props，不改 Media/Window producer 或 UI 行为。若 runtime guard 收窄 null 支持，可同步收窄 Props 回滚。

## 验收

Renderer contract tests 覆盖 root/nested optional null、错误 row 值回退与 `content` 动态载荷；运行 Svelte type check、UI 全量测试与 ADR 索引检查。
