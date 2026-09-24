# ADR 0291：会话删除与清空通过 SessionStore

- 状态：Accepted
- 日期：2026-09-24
- 范围：Agent `SessionSupervisor` 单会话删除与历史清空的 durable Database 调度
- 关联：[ADR 0157](0157-session-supervisor-actor-run-engine.md)、[ADR 0172](0172-session-action-lifecycle-state-contract.md)、[ADR 0260](0260-session-store-session-creation-port.md)、[ADR 0290](0290-session-status-persistence-through-session-store.md)

## 背景

`SessionSupervisor::delete_session` 和 `clear_sessions_and_delete` 仍直接在 Agent 中调用 `Database::run_blocking`，而 supervisor 已持有共享的 `SessionStore`。这两条 durable 写路径因而重复暴露 blocking-pool 调度边界。

删除与清空还包含 Agent 所有的 lifecycle gate、run quiesce 和 actor 清理。本次只收口最后的 durable Database 调度，不改变生命周期编排或底层 SQL/事务。

## 决策

1. 为 `SessionStore` 增加异步 `delete_session` 与 `clear_sessions` 端口。端口只在 blocking pool 调用既有 `Database::delete_session` / `Database::clear_sessions`，并原样返回结果、错误和清空计数。
2. `SessionSupervisor` 的两条写路径改为调用上述端口。Agent 保留现有 raw `Database` 字段，因为其他路径仍需要它；不新增通用 storage trait 或下沉 lifecycle policy。
3. 单会话删除顺序不变：`begin_session_closing` 建立 closing 标记并取消 direct waiter；`quiesce_session` 取消并等待 run；取得 lifecycle gate 后，`remove_session_locked` 清除 actor runtime、pending 项、工具 overlay、授权信任、scheduled confirm 和 actor；最后删除 durable row。删除缺失 row 仍返回 `session '<id>' not found in database`；如果数据库删除失败，已完成的内存清理不回滚。
4. 全量清空顺序不变：先 block 新 lifecycle 操作，quiesce 全部 session，再取得 lifecycle gate 并由 `clear_all_sessions_locked` 清空 actors、pending queue、scheduled confirms、direct waiters 和授权信任；最后调用原子 `Database::clear_sessions`。其删除行计数和失败传播保持不变；持久层失败时此前完成的内存清理不回滚。
5. 两个端口直接复用原 Database 方法，因此单删的级联、KV、embedding 与 cache cleanup，以及全清的事务、KV、embedding 与 cache cleanup 均保持原实现。调用方 future 被丢弃时，已经启动的 blocking write 不会因此中断，与原 `run_blocking` 调用一致。

新增更高层生命周期封装会把 Agent policy 移入 Memory；改变 Database 清理实现则扩大了本切片的持久化风险。两者都不符合本次窄端口目标。

## 影响与验证

- 无 schema、IPC 或用户数据契约变化，无需重置数据库。
- Memory 304 项与 Agent 498 项定向测试通过。新 Memory 测试覆盖删除清理、原 not-found 错误、全清清理和精确计数；Agent 测试覆盖单删的 actor/持久状态、全清的内存 lifecycle 与 durable state，以及返回计数。
- 通过：`cargo fmt --all -- --check`、`cargo check --workspace --locked`、`cargo clippy --workspace --locked -- -D warnings`、`cargo test --workspace --locked --quiet`。workspace 测试全部通过；其中 Tools 测试套件为 733 项通过、2 项忽略。

## 回滚

恢复 `SessionSupervisor` 中原有的两处 `Database::run_blocking` 调用，并删除 `SessionStore::delete_session` / `clear_sessions`、对应测试、本 ADR、索引与路线图记录。无需迁移或重置数据库。
