# ADR 0317：后台与定时任务共享终态内核

- 状态：Implemented
- 日期：2026-09-25
- 范围：`haven-tools::ActionService` 的 background/scheduled 终态构造与进程内仲裁
- 关联：[ADR 0248](0248-background-action-terminal-commit-order.md)、[ADR 0259](0259-memory-runtime-committed-event-consumer.md)、[ADR 0267](0267-memory-runtime-maintenance-schedule.md)、[ADR 0305](0305-action-service-action-store-port.md)

## 现状审查与不变量

实现前检查了 `action_service.rs`、`action_lifecycle.rs`、`ActionStore` 及现有生命周期测试。`action_lifecycle.rs` 只共享事件 sink；后台进程和定时触发仍是两条独立生命周期。以下边界是本切片必须保留的行为：

1. 终态只能从 live 状态认领；后台与 scheduled 完成/失败要求当前为 `running`，scheduled 取消还允许 `waiting`。已终态状态不能被第二次完成、失败或取消覆盖。
2. `started_at` 原样传入终态；`finished_at` 在首次构造候选时取 UTC RFC3339，并由写失败的重试继续复用。取消尚未触发的 scheduled action 时，内存 `started_at` 为空、durable 列为 `NULL`，事件继续省略该字段。
3. 后台完成/失败的 ActionStore 条件 CAS 与 completion outbox 在同一事务内提交。只有 CAS 胜者才投影终态并发布 `action:finished` / transient completion；取消不写 outbox。CAS 输家静默对齐 durable row，存储错误保留候选并按既有退避重试。
4. 有 durable row 的 scheduled fire 仍先持久化 `waiting → running`，再更新内存、发 `action:updated` 并发送 completion bus；watch-action dependency 保留原有 runtime-owned 分支。没有消费者时，先尝试 durable `running → waiting` 回滚，再恢复内存 Waiting 并重装 timer；回滚失败则保留 fire 供迟到 consumer 恢复。有 durable row 时 scheduled 终态 CAS 成功后才更新内存并发 `action:finished`；watch-action dependency 保留原有跳过这些持久化转换的行为。scheduled 不进入后台 completion outbox。
5. `spawn_gate` 继续保护 scheduled admission、fire、cancel 和 completion 的外部生命周期；进程 kill、scheduled fire claim 清理、timer 重装、ActionStore 调用、outbox ack 和事件发布各自留在原有调用链。

## 决定

新增 crate-private `action_terminal.rs`，由 `ActionState`、`TerminalPayload` 和 `TerminalTimestamps` 统一终态构造；`can_claim_terminal` 统一判断来源状态与目标是否可认领，`TerminalTransitionGuard` 串行化同一 `ActionService` 内终态提交期间的状态检查、持久化和内存投影。

后台 `mark_finished` / `mark_cancelled` 与 scheduled `finish_scheduled` / `cancel_scheduled` 复用该内核。两类任务的持久化 CAS、错误重试、进程/timer/consumer 行为和发布顺序仍由各自路径管理。该切片没有修改 schema、ActionStore API、IPC JSON、事件 payload 或用户可见状态。

## 验证

- 共享内核测试覆盖 completed/failed/cancelled payload、原样 started/finished 时间戳、UTC RFC3339 时间生成、Waiting 只能取消及重复终态拒绝。
- ActionService 测试覆盖 scheduled 重复完成/失败/取消只发布一次，并保留 waiting 取消的空 `started_at` 行为。
- 既有测试覆盖 scheduled 无 consumer 回滚与重装 timer、回滚失败后的迟到 consumer recovery、scheduled 终态持久化重试、后台 CAS 竞态/输家静默、outbox 事务与存储错误重试。
- 通过 `cargo fmt --all -- --check`、`cargo test --locked -p haven-tools`、`cargo check --workspace --locked`、`cargo clippy --workspace --locked -- -D warnings` 和 `cargo test --workspace --locked`。

## 未完成工作与回滚

这不是统一 Job 生命周期的完成标记。trigger/execution 建模、共享 claim/lease、timeout 与 retry policy、tail output 以及统一 UI projection 仍待单独设计和切片；messaging 仍是独立 transport domain。MemoryRuntime 及 MemoryWorker 的职责迁移按 Phase 7 原计划继续推进。

回滚本 ADR 对应的 ActionService/内核代码和测试即可；没有数据库迁移、IPC/config 重置或用户数据操作。完整 Job 生命周期如需更改持久化或对外契约，需另行补充决策。
