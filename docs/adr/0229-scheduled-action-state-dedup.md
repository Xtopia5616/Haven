# ADR 0229：定时任务状态事实去重

- 状态：已采纳（2026-09-24）
- 范围：`haven-tools` 的 `ActionService` 定时任务运行态
- 关联：[ADR 0172](0172-session-action-lifecycle-state-contract.md)、[ADR 0174](0174-action-refresh-and-scheduled-delivery-claims.md)、[ADR 0215](0215-action-board-typed-view.md)

## 背景

定时任务的会话归属同时存在于 `ActionEntry` 和 `ScheduledActionEntry`，等待状态又重复携带 `due_at`。创建、恢复、取消和触发路径必须分别同步这些字段，容易产生投影不一致。

## 决定

1. `ActionEntry.session_id` 是所有 action 运行态的唯一会话归属来源。
2. `ScheduledActionEntry.due_at` 是定时任务触发时间的唯一来源；`ActionState::Waiting` 不再携带时间载荷。
3. status、board、事件和恢复路径从这两个唯一来源重新组装既有 JSON/typed view，保持字段名和生命周期语义不变。
4. 后台 action 与定时 action 仍保留不同的触发、执行、完成投递和取消语义；`ScheduledActionFired.session_id` 作为既有投递载荷保留。
5. 不修改数据库 schema、IPC 或 Agent 投递契约；只删除进程内重复字段及其同步路径。

## 替代方案

- 保留两份 session/due_at：会继续扩大状态同步面，拒绝。
- 现在合并后台与定时任务的完整 Job 生命周期：风险和写集过大，留待阶段 7 的独立审查。

## 影响与验证

定时任务恢复、取消、触发、无消费者回退和完成事件只从单一运行态来源读取事实。验证包括 `cargo fmt --all -- --check`、`cargo test --locked -p haven-tools` 和 `cargo check --workspace --locked`。

## 回滚与重置

无持久化格式变化，不需要数据重置。回滚时恢复 `ScheduledActionEntry.session_id` 与 `ActionState::Waiting { due_at }`，并移除此 ADR 和索引项。
