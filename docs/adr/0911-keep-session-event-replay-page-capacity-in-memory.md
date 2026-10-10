# 0911：将 Session event replay 页容量保留在 Memory

## 状态

已接受并实现（2026-10-11）。

## 背景

`SessionStore::replay_page_cancellable` 在 Memory 内执行 SQLite 查询并校验 replay 页大小，但生产 Agent `MemoryRuntime` 仍要导入 `MAX_SESSION_EVENT_REPLAY_PAGE_SIZE`，并在每次调用时传入该值。该限制属于存储查询的资源边界，Agent 的恢复循环只需要推进 session、cursor 和取消状态。

这使 Agent 依赖 Memory 内部查询容量，也与 ADR 0907 将持久化分页边界收回 typed store 的原则不一致。`SessionStore` 内部同步 replay helper 已私有，且 replay 页的细粒度游标测试在 Memory 自身测试中完成；生产调用没有选择不同页大小的需求。

## 决定

- `SessionStore::replay_page_cancellable` 不再接受调用方传入 `limit`，每次固定使用 Memory 内部的 `MAX_SESSION_EVENT_REPLAY_PAGE_SIZE`。
- replay 页大小常量改为 `session_events` 模块私有，不从 `haven_memory` crate root 导出。
- Agent `MemoryRuntime` 只传 `session_id`、`after_sequence` 与 cancellation token；恢复循环仍由 Agent 拥有。
- 保留私有同步 helper 的 limit 参数，供 Memory 单元测试验证小页分页、游标和上限拒绝行为；测试实现参数不进入生产 API。

## 影响

下游不能再自行选择 Session event replay 容量，Memory 成为唯一资源上限 owner。单页最大行数、顺序、cursor、高水位、取消、重放和事件持久化行为不变；不改变 schema、event payload、IPC 或用户可见行为，无需数据重置。API 变更不保留兼容重载或别名。

回滚时恢复公开常量与 `limit` 参数即可，无持久数据回滚步骤。

## 验证

- `cargo fmt --all -- --check`
- `cargo check --workspace --locked`
- `cargo test --workspace --locked`
- `cargo clippy --workspace --locked -- -D warnings`
- `pwsh -NoProfile -File scripts/check-adr-index.ps1`

## 替代方案

- 继续由 Agent 传入 Memory 常量：拒绝，因为 Agent 不拥有 SQLite 查询容量策略。
- 把 recovery loop、cursor 或 high-water 移入 Memory：拒绝，因为这些属于 Agent runtime 的生命周期与恢复编排。
- 删除同步 helper 的测试 limit：拒绝，因为单页容量边界与小页游标推进仍需要由 Memory 单元测试直接验证。
