# 0914：按 ToolRun 持久化域命名 Memory 底层模块

## 状态

已接受并实现（2026-10-11）。

## 背景

`crates/memory/src/repositories/scheduled_tool_runs.rs` 除 scheduled ToolRun 的触发、取消、完成与恢复查询外，还定义通用 `ToolRunRow`、依赖结果投影，并实现 background ToolRun 写入、终态事务、列表、读取与清理。它是 `tool_runs` 持久域的底层 SQLite 实现；跨 crate 的异步存储边界已单独由 `ToolRunStore`/`tool_run_store.rs` 提供，completion delivery outbox 也有独立 owner 文件。

`scheduled_tool_runs.rs` 会让维护者误以为文件只含定时任务。实际下游消费由根导出的 typed API 完成，底层文件名不构成外部契约，因此没有保留旧名称的必要。

## 决定

- 将 Memory 内部文件和模块 `scheduled_tool_runs.rs` / `scheduled_tool_runs` 改名为 `tool_runs.rs` / `tool_runs`，与其覆盖的持久化实体范围一致。
- 保留 `ToolRunStore` 作为跨 crate typed async façade，继续负责 blocking-pool 调度；保留 `tool_run_completion_outbox.rs` 作为终态结果投递记录实现。
- scheduled 专属查询和 lifecycle 操作仍留在同一底层模块；此切片只校正模块范围名称，不拆分单一 `tool_runs` 表的事务与行映射。
- 更新 Memory root re-export 与 facade 的内部模块引用。Scheduled API、`ToolRunRow`/`ToolRunDependencyRow` DTO、SQL、schema、运行时行为、IPC 和持久化格式均不变。

## 影响

这是 Memory crate 内部模块改名；无新增或删除跨 crate API，无数据迁移或重置，也不改变 ToolRun 状态、调度、恢复、事务和 completion delivery 语义。

## 验证

- `cargo fmt --all -- --check`
- `cargo check --workspace --locked`
- `cargo test --workspace --locked`
- `cargo clippy --workspace --locked -- -D warnings`
- `pwsh -NoProfile -File scripts/check-adr-index.ps1`

## 替代方案

- 保留 `scheduled_tool_runs.rs`：拒绝，因为它已实现 background 与通用 ToolRun 持久操作，名称只覆盖了其部分范围。
- 改名为 `tool_run_rows.rs`：拒绝，因为文件不仅定义行 DTO，还拥有这些行对应的 SQLite lifecycle 与查询操作。
- 把 scheduled/background 按 kind 拆成两个底层文件：暂不采用；它们共享一张 `tool_runs` 表、通用行查询、终态 outbox 原子性和清理约束，按 kind 拆开会跨文件分散同一持久化不变量。

## 回滚

将文件、模块声明、引用与根导出恢复原名即可；没有持久化恢复步骤。
