# ADR 1122：按工具结果持久化定时工具终态

## 背景

`SessionSupervisor::execute_gated` 将工具拒绝、策略阻止和工具自身失败表示为 `Ok(ToolExecution)`，让 ReAct 能把失败作为普通工具观察继续处理。定时工具执行路径此前只检查外层 `Result::is_ok()`，因此 `ToolResult.success == false` 也会被记录为 `completed`。确认执行路径还忽略持久化终态操作返回的 CAS 结果，并在持久化前发布通知；状态转换失败或另一终态已先胜出时，用户仍可能收到错误或重复通知。

## 决定

1. 定时工具执行只有在 `ToolExecution.confirmed != Some(false)` 且 `ToolResult.success` 为 `true` 时才进入 `completed`；外层 `Ok` 本身不代表业务执行成功。失败结果摘要作为 `failed` 原因保存。
2. 自动批准和用户确认后的定时工具执行共用该成功判定。
3. 定时工具完成/失败通知只在对应 `complete_scheduled*` 或 `fail_scheduled` 返回 `Ok(true)` 后发布。返回 `Ok(false)` 表示终态 CAS 未胜出；返回错误表示持久化未成功；两种情况都不发布终态通知。
4. `execute_gated` 仍通过 `Ok(ToolExecution)` 向普通 ReAct 调用方传递工具失败观察，不改变其通用结果契约。

## 替代方案

- 把所有 `ToolResult.success == false` 改成 Rust `Err`：会改变 ReAct 对工具失败的既有观察契约。
- 保留外层 `Result` 作为定时任务状态依据：无法表示执行层已经以正常 observation 返回的工具失败。
- 先通知再尝试持久化：会使通知与 durable ToolRun 终态不一致。

## 影响

定时工具失败会显示为 `failed`，其摘要进入现有有界失败原因字段。只有成功赢得终态状态转换的执行才发布对应通知。没有数据库 schema、IPC 或配置字段变化，不需要重置现有数据。

## 验证

- 回归测试覆盖用户确认后，外层执行成功但工具结果失败时持久状态为 `failed`。
- 回归测试覆盖终态 CAS 未胜出时不再发布第二条完成事件。
- 运行 `cargo fmt --all -- --check`、`cargo test --locked -p haven-agent` 和 `cargo clippy --locked -p haven-agent -- -D warnings`。

## 回滚

回滚仅恢复代码行为，会重新引入错误的完成状态和通知。此变更没有迁移或数据回写；回滚后可按现有 ToolRun 管理功能处理已存在的终态记录。
