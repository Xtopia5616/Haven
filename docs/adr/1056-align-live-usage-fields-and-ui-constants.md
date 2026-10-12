# ADR 1056：统一 live usage 字段与 UI 静态选项常量命名

## 状态

已接受（2026-10-12）

## 背景

Session resume 的用量行保留 Rust `LlmUsageRecord` 的 snake_case 字段；live Agent usage handler 将同类投影放入同一个 reducer collection 时，却把 `request_kind` 单独写成 `requestKind`。另外，若干静态选项和固定标签映射仍使用 camelCase 变量名，与 `docs/naming.md` 的 UPPER_SNAKE_CASE 规则不一致。

## 决定

- live usage reducer row 沿用 `SessionLlmUsage` 的 Rust-owned snake_case shape，写入 `request_kind`；Agent event 到 renderer 的 camelCase 转换仍由 event contract 边界负责。
- UI 静态词表、固定选项及固定操作标签映射使用 UPPER_SNAKE_CASE。动态派生值、组件 props 和普通局部变量继续使用 camelCase。
- 测试明确验证 live usage 行包含 `request_kind` 且不包含额外的 `requestKind` 字段。

## 影响

Reducer 内 live 与 resume 来源的 usage row 形状一致；无 IPC、数据库或持久化格式变化。UI 静态常量名称调整不改变选项内容或行为。

## 验证

- `chatUsageEventHandlers` 测试覆盖 live usage 投影字段。
- UI 全量测试、Svelte 类型检查和生产构建通过。
