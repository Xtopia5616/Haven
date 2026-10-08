# ADR 0785：对齐 Agent renderer root Props 的 null 语义

## 状态

已采纳并实施（2026-10-08）。

## 背景

AgentTool 多个输出通过 `json!` 投影来自 `Option` 的 role、title、status metadata 和 session fields。Agent renderer 的 optional-field guard 将 null 当作缺省值，但 root Props 对 auto/text、timeout、session、role 与 queue metadata 等只声明了非空可选类型。nested `agents[]` 的 name/status 则是必需字段，title/role 可空。

## 决定

1. 对齐 Agent root optional scalar Props 与 guard：guard 接受 null 的字段显式声明 `| null`；动态 `reply` 继续为 `unknown`。
2. 保持 `agents` 为非 null 数组、row `name` 与 presence `status` 必需，title/role nullable；错误 row 继续回退通用 JSON renderer。
3. 不把自由文本 role 收窄为 enum，因 producer 的 role 为用户定义 discovery token。

## 影响与回滚

仅修正动态 ToolResult 边界 Props，不改 messaging producer 或跨端数据。若 guard 调整为拒绝 null，可同步收窄 root Props 回滚。

## 验收

Renderer contract tests 覆盖 null optional root fields 保持专用 renderer、错误 scalar 类型与错误 presence status 回退；运行 Svelte type check、UI 全量测试与 ADR 索引检查。
