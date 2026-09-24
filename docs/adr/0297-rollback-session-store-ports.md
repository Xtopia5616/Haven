# ADR 0297：Rollback 事务通过 SessionStore 异步端口

- 状态：Accepted
- 日期：2026-09-25
- 范围：`rollback.rs` 的 rollback target 预校验、timeline rollback 事务与 continue recovery projection 截断
- 关联：[ADR 0206](0206-session-store-usage-events-and-atomic-rollback.md)、[ADR 0207](0207-session-store-replay-boundaries-and-durable-ui-sequences.md)、[ADR 0243](0243-committed-recovery-truncation.md)、[ADR 0296](0296-react-transcript-event-store-ports.md)

## 背景

rollback 的目标消息预校验、最终 `rollback_to` 事务及 continue 的 committed-recovery projection 截断都已有同步 `SessionStore` 操作，但 `AgentLayer::rollback_session` / `continue_session` 仍直接通过 raw `Database::run_blocking` 调度它们。Agent 已持有该 SessionStore，重复调度使 rollback 继续暴露存储边界。

rollback 的生命周期 cancel/join、事件 replay、compaction boundary、branch/event trimming、工具恢复与成功提交后的 usage/status 更新都属于 Agent 编排。Memory 只负责按 session/message id 读取目标行，并在 blocking pool 调度已有同步查询或事务；不接管策略判断。

## 决策

1. `SessionStore` 增加三个具体异步端口：`load_rollback_target_message`、`rollback_to_async` 与 `truncate_projection_after_latest_committed_recovery_async`。它们都使用 `Database::run_blocking` 并复用现有同步方法，不增加 trait、通用 facade 或依赖。
2. 目标消息端口通过既有 `Database::get_message_by_id(session_id, message_id)` 执行 session-scoped 精确查询。缺失或跨 session 的 id 继续返回 `rollback target message '<id>' not found in session messages`；Agent 仍负责判断角色、orphan 状态及是否能在恢复 transcript 中找到该用户消息。预校验仍发生在 lifecycle cancel、interaction 清理等副作用之前；`rollback_to` 仍在事务内重新验证投影边界。
3. `rollback_to_async` 将 owned `RollbackRequest`、完整 replacement transcript 与 run id 一起传入一个 blocking closure，并整体调用既有 `rollback_to`。marker、projection 截断、replacement transcript append、cursor 解析仍属于同一事务；过期 event boundary 继续 fail closed，rollback marker 与 replacement root 只在提交后广播。
4. continue 通过 `truncate_projection_after_latest_committed_recovery_async` 调度现有单事务操作。全历史最新 recovery marker 与 phase 检查、active branch cutoff、投影删除、usage 补偿和 aggregate 重建仍由 SessionStore/Database 共同执行。
5. Agent 保留 lifecycle cancel/join、event replay、compaction boundary 判断、branch/event trimming、partial discard、tools restore、usage invalidation、interaction 清理和最终 status 更新。事务失败会在已有顺序点直接返回，不执行 rollback 成功后的 usage invalidation、工具恢复或 status 更新；continue 截断失败也不继续清理 partial/interactions 或变更 status。
6. 三个端口保持普通 `run_blocking` 语义：future 被丢弃不会中断已经运行的 blocking 数据库工作。此切片不改为 cancellable 调度，也不扩大到 resume attachments、compaction summary 或 Tools/UI。`rollback.rs` 不再为这三条路径直接调度 raw Database；`AgentLayer`、`ReActEngine` 及其他模块仍因各自未迁移职责保留 raw Database 字段/调用。

让 Memory 解析 transcript、决定 lifecycle 或恢复策略会反转职责；在 Agent 重复套用 blocking 调度则继续暴露存储细节。窄端口只负责现有的 session/message 查询与事务调度。

## 影响与验证

- 无 schema、IPC、持久数据契约或依赖变化，无需数据库重置。
- Memory 测试覆盖 session-scoped target validation、replacement transcript 与 rollback marker 的事务顺序、过期 cursor fail-closed，以及 committed recovery projection 截断异步端口。
- Agent 回归覆盖 target 缺失时不触发生命周期副作用、rollback transaction 失败时不更新 Agent status/projections，以及 continue committed recovery 截断的投影结果。
- 验收命令：`cargo fmt --all -- --check`、`cargo test --locked -p haven-memory`、`cargo test --locked -p haven-agent`、`cargo check --workspace --locked`、`cargo clippy --workspace --locked -- -D warnings`、`cargo test --workspace --locked`。

## 回滚

将三个 Agent 调用恢复为原有 `Database::run_blocking` 包装，并删除三个新增异步端口、对应测试、本 ADR、索引与路线图记录。无需迁移或重置数据库。
