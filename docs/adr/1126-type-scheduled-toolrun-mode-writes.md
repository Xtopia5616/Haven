# ADR 1126：用闭合枚举约束定时 ToolRun mode 写入

## 背景

定时 ToolRun 的 mode 只允许 `tool` 与 `continue`。Tools 已有 `ScheduleMode` 闭合 enum，restore 也会把未知的持久值隔离为失败行，但 Memory 同步/异步写入 API 仍接收任意 `String`。调用者拼错或提前写入未来值时，数据库会接受该行，直到下一次 restore 才能发现。

## 决定

1. 将唯一 `ScheduleMode` 定义移入 `haven_common::tool_run`，由 Tools 重导出以保留既有便利入口。
2. `Database::save_scheduled_tool_run` 与 `ToolRunStore::save_scheduled_tool_run` 均只接收 `ScheduleMode`，并在 SQLite 文本列边界调用 `as_str()`。所有生产调用点与有效测试数据使用 enum。
3. 持久化读取仍保留原始 `mode: String`。restore 和 UI projection 各自通过同一 Common enum 的解析入口处理历史值；restore 对未知行保持 quarantine 语义。
4. 不改 schema 版本，也不增加 SQL `CHECK`。这次收紧应用写入契约；手工修改、损坏或旧库中的未知值仍通过已有恢复隔离路径处理。

## 替代方案

- 继续让各层传 `String` 并在每个写入口重复解析：验证位置容易漂移，调用者仍可绕过。
- 增加 SQLite `CHECK` 并重置数据库：会把本次 API 收口扩大为 schema 不兼容变更，当前没有必要，因为非法历史值已有 fail-closed quarantine。
- 把 enum 留在 Tools 并让 Memory 依赖 Tools：会反转 crate 依赖方向，破坏 Memory 的领域边界。

## 影响

生产调用者在编译期选择合法 mode。SQLite 列仍为文本，因此现存数据与 IPC JSON 不变。未知历史值仍可被读取为原始文本并按原有策略隔离；不需要迁移或重置用户数据库。

## 验证

- Common 单测覆盖 enum 的 JSON 与持久字符串词汇以及未知值拒绝。
- 恢复测试通过原始 SQL 注入未知历史值，确认 quarantine 与重试行为仍有效。
- 运行 workspace Rust tests、clippy 与格式检查。

## 回滚

可恢复 String 写入签名并将 `ScheduleMode` 留在 Common 或 Tools；不需要 schema 迁移。回滚会重新允许新的非法 mode 写入。
