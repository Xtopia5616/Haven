# ADR 0778：Files 与 Input renderer 复用闭合输出值

## 状态

已采纳并实施（2026-10-08）。

## 背景

Files ToolResult 的 `operation` 由闭合 `FilesOperation` 写入；outline 的 symbol `kind` 来自 `file_outline::outline_symbol` 内的固定 declaration-kind 列表。Input ToolResult 的 `operation` 来自 `InputOperation`，鼠标 `button` 来自 `InputButton::as_str()`。UI props 与 nested guards 曾把这些 renderer discriminator 全部视为任意字符串。

## 决定

1. UI `ToolFileOperation`、`ToolFileSymbolKind`、`ToolInputOperation` 和 `ToolInputButton` tuple 与 producer 值域一致。
2. File/Input renderer props 和 `toolResultValidation` 共用 presentation 类型及 runtime guard；其它自由文本字段保持 string。
3. 未知 operation、symbol kind 或 button 回退通用 JSON renderer，保留原始 ToolResult payload。Input 的 `type_element` / `click_element` 虽走该组件的通用 JSON 展示分支，仍属于合法 producer operation。

## 影响与回滚

只收紧 UI builtin renderer 内部的展示契约，不改变 Files/Input ToolResult JSON、Tauri IPC、持久化或输入副作用。未知历史值仍可通过通用 JSON 查看。移除 tuple/guard 并恢复 props 字符串类型即可回滚。

## 验收

UI contract tests 覆盖合法 outline kind、Input element operation/button，以及未知 operation/kind/button 的 JSON fallback；执行 Svelte type check、UI 全量测试与 ADR 索引检查。
