# ADR 1134：将 ToolRun 删除 helper 限定为私有并使用闭合 kind

## 背景

`ToolRunService::delete` 接收 `kind: &str`，再把 `background` / `scheduled` 解析回已有的 `ToolRunKind`。全仓调用点只有 `delete_terminal` 和同 crate 测试；对外的终态删除能力已经由 `delete_terminal` 提供。字符串参数使内部 helper 重复表达了持久化词汇表，也扩大了无实际消费者的公开面。

## 决定

- 将 `delete` 收窄为私有 helper，参数使用 `ToolRunKind`；保留 `delete_terminal` 作为终态历史删除入口。
- 只在读取持久化 row 的边界将闭合 `ToolRunKind` 映射到存储字符串。删除路径遇到未知持久化 kind 时 fail closed，并保留该行。
- 保留现有终态、缺失行及 kind 不匹配的拒绝语义。

## 替代方案

- 保留公开字符串 helper：拒绝。全仓没有 helper 的跨 crate 消费方，且它重复解析已由 `ToolRunKind` 表达的闭合集合。
- 让存储层接受 `ToolRunKind`：拒绝。本决定不改变存储契约；数据库 row 在存储边界仍使用文本。

## 影响与验证

这是 Tools crate 内部 API 的收窄和类型澄清，不改变 ToolRun 行为、IPC、数据库 schema 或持久化格式，不需要迁移或重置。未知持久化 kind 的回归测试确保删除被拒绝且行仍存在；同时运行 Tools crate 测试与严格 Clippy。

## 回滚

可恢复原有字符串 helper 和 `delete_terminal` 的字符串映射。没有需要迁移或清理的持久数据；回滚时同步恢复对应的类型与 fail-closed 测试。
