# ADR 0977：删除无界 ToolRun 原始行列表接口

## 状态

已采纳并实施。

## 背景

`ToolRunStore::list_tool_runs` 向下游暴露了一个不带 owner 或 page limit 的原始 `ToolRunRow` 列表。全仓没有生产调用，Memory 自身只有一个用于确认空结果的测试调用。生产 board hydration 和 history 已有职责明确的具名查询，Tools 再将结果投影成 `ToolRunView`。

## 决定

删除无界 `ToolRunStore::list_tool_runs` facade 方法，并将底层全量 repository helper 限制到 `test` / `test-support`。保留按 session 读取的 ToolRun 查询，因为运行恢复路径实际使用它；保留限页终态 history 查询和 board hydration 专用查询。

## 替代方案

- 将无界列表方法改为 `test-support` API：拒绝；没有测试需要它，现有测试可以通过验证写入后数据库仍为空来确认持久化前校验。
- 为该方法增加全局 page：拒绝；当前没有调用方需求，新增另一套跨类型分页入口只会扩大 API。

## 影响与验证

- Memory typed store 不再向生产消费者提供无 owner、无界的原始 ToolRun 列表；底层全量 helper 只供测试使用。
- 全仓编译与 Memory/Tools 测试验证保留的具名查询及调用路径。
- 不改 SQLite 查询能力、ToolRun 数据或数据库 schema。

## 回滚

恢复该 typed-store facade 方法和测试断言，并同步回退路线图与 ADR 索引；不涉及数据迁移。
