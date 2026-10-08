# ADR 0789：对齐 System renderer 的嵌套 Props null 语义

## 状态

已采纳并实施（2026-10-08）。

## 背景

`validSystemData` 使用 optional-field/object guards 验证 system info 与 collection rows：scalar 及 optional record fields 的 null 按缺省处理，record/array item 与显式 string arrays 保持结构约束。`ToolSystemResult.Props` 原先多处只允许 undefined，未表示 guard 允许的 null。

## 决定

1. 对齐 root scalar、`os/user/locale/cpu/memory/network_summary` record 与 network/disk/display/environment row 的 optional fields；record 本身也允许 null。
2. 保持 `networks[].ips`、`values`、`subkeys` 与 collection arrays 的非 null 数组契约。
3. `networks[].state` 继续是开放文本；scope、电源状态继续用现有闭合集合。

## 影响与回滚

只修正动态 system ToolResult renderer Props，不改 SystemTool 输出或 Tauri 边界。若 guard 收窄 null 语义，可同步收窄 Props 回滚。

## 验收

Renderer contract tests 覆盖 root/nested null 与错误 object field fallback；运行 Svelte type check、UI 全量测试与 ADR 索引检查。
