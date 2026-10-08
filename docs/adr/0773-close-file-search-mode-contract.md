# ADR 0773：File search mode 闭合生产与 renderer 契约

## 状态

已采纳并实施（2026-10-08）。

## 背景

`files.search` 的输入 schema 声明 `filename` / `content`，但 `FilesParams`、`SearchOptions` 和 `SearchRequest` 曾把 mode 保留为任意字符串；producer 将该值写入异构 `ToolResult.output`。UI renderer 同样将嵌套 mode 当作开放字符串，未知值会被静默显示为“文件名”。

## 决定

1. Rust `FileSearchMode` 闭合为 `Filename` / `Content`，JSON 表示为 `filename` / `content`。
2. 两个入口（`FilesTool` 参数与 JSON `SearchRequest`）都在执行前解析并拒绝未知或非字符串 mode；缺省仍是 `filename`。
3. UI 用 `ToolFileSearchMode` 描述 renderer prop，并由同一 guard 验证动态结果。未知历史/外部结果回退通用 JsonView；mode 继续可省略以兼容结果缺字段时的默认展示。

## 验收与回滚

Rust 单测覆盖缺省、两种有效值及未知值拒绝；UI contract test 覆盖未知动态 mode 的 renderer fallback。运行 Tools crate 测试与 Clippy、UI type check 与全量测试。无 IPC 或持久 schema 变更；回滚 enum、入口解析与 renderer guard 可恢复开放字符串行为。
