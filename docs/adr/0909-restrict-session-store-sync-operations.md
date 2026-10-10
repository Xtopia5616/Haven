# 0909：将 SessionStore 同步 SQLite 操作限制在 Memory 与测试边界内

## 状态

已接受并实现（2026-10-11）。

## 背景

`SessionStore` 是 Memory 根导出的 typed store，但其中一部分同步方法直接获取 SQLite connection。即使它们主要供 `run_blocking` closure、Memory 单元测试或 Agent 集成测试使用，只要在默认生产 API 中保持 `pub`，下游仍能绕过 blocking/cancellation 边界，直接追加任意事件、回放原始历史或执行 rollback/projection 原语。

调用图核对显示，生产 Agent/App 路径已使用 typed async/cancellable store 操作；原始同步入口的跨 crate 调用仅出现在测试 fixture 与断言中。`append_usage` 等仍有生产消费者的能力不属于本次收口范围。

## 决定

- SessionStore 内部 SQLite 同步实现改为私有；只供同一 repository 子树测试共享的 `append_batch` 限为 `pub(super)`。这包括原始 event append、transcript commit/append、replay 查询、seed、rollback、branch/recovery marker helper 和 projection truncation。
- 生产消费者保留 typed async/cancellable 操作，由 Memory owner 在 blocking pool 中调用同步实现。`SessionStore` 的 `append`、`commit_transcript`、`append_transcript`、`append_branch_point`、`append_recovery_persistence`、`latest_recovery_persistence`、`latest_sequence`、`read_all`、`read_active_transcript`、`read_active_domain_events` 与 cursor bootstrap 同步 fixture API 仅通过 `test-support` feature 跨 crate 可见；底层 `Database` cursor initializer 为 crate-private。
- `clear_memory_event_cursor` 只留在 Memory 自身单元测试构建中；未使用的 SessionStore cancellable initializer 已删除。Agent fixture 改为通过 SessionStore test-support port 初始化 cursor，不再直接调用 raw Database。
- 保留有生产消费者的 typed write/read 方法，不改变事件校验、事务原子性、回滚双时钟或广播顺序。

## 影响

默认生产构建不再允许下游同步触达这些 SQLite 原语，也不能以通用 `append` 直接构造任意事件。测试 fixture 仍能通过非默认 `test-support` 使用必要的同步观察和构造能力，且不必调用 raw Database 的 cursor 写方法。

不改变数据库 schema、持久化数据、事件 payload、IPC 或运行时行为；无需数据重置。需要回滚时恢复相应方法可见性，并重跑本 ADR 的验证门禁。

## 验证

- `cargo fmt --all -- --check`
- `cargo check --workspace --locked`
- `cargo test --workspace --locked`
- `cargo clippy --workspace --locked -- -D warnings`
- `pwsh -NoProfile -File scripts/check-adr-index.ps1`

## 替代方案

- 继续公开同步实现并要求调用方自律：拒绝，因为 Rust API 仍允许生产消费者绕过 SQLite blocking boundary。
- 把所有同步事务搬到 Agent：拒绝，因为事务、projection 与 live event 发布是 Memory `SessionStore` 的持久化不变量，向外搬会泄漏实现并拆开唯一 owner。
- 删除所有同步测试能力：拒绝，因为 Memory/Agent 测试需要构造边界故障并检查 append-only timeline；使用非默认 `test-support` 可保留这些验证能力而不扩大生产契约。
